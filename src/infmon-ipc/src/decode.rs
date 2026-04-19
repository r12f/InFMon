use std::net::{IpAddr, Ipv6Addr};

use crate::error::IpcError;
use crate::types::*;

/// Decode raw key bytes into a vector of FieldValues according to the field list.
pub fn decode_key(fields: &[FieldId], key_bytes: &[u8]) -> Result<Vec<FieldValue>, IpcError> {
    let mut offset = 0;
    let mut values = Vec::with_capacity(fields.len());

    for field in fields {
        match field {
            FieldId::SrcIp | FieldId::DstIp | FieldId::MirrorSrcIp => {
                if offset + 16 > key_bytes.len() {
                    return Err(IpcError::StatsFormat(format!(
                        "key too short for {:?} at offset {}: need 16 bytes, have {}",
                        field,
                        offset,
                        key_bytes.len() - offset
                    )));
                }
                let bytes: [u8; 16] = key_bytes[offset..offset + 16].try_into().map_err(|_| {
                    IpcError::StatsFormat(format!(
                        "failed to convert {:?} bytes at offset {}",
                        field, offset
                    ))
                })?;
                let ip = decode_ip(bytes);
                values.push(FieldValue::Ip(ip));
                offset += 16;
            }
            FieldId::IpProto => {
                if offset >= key_bytes.len() {
                    return Err(IpcError::StatsFormat(format!(
                        "key too short for IpProto at offset {}",
                        offset
                    )));
                }
                values.push(FieldValue::Proto(key_bytes[offset]));
                offset += 1;
            }
            FieldId::Dscp => {
                if offset >= key_bytes.len() {
                    return Err(IpcError::StatsFormat(format!(
                        "key too short for Dscp at offset {}",
                        offset
                    )));
                }
                values.push(FieldValue::Dscp(key_bytes[offset]));
                offset += 1;
            }
        }
    }

    if offset != key_bytes.len() {
        return Err(IpcError::StatsFormat(format!(
            "key has {} trailing bytes",
            key_bytes.len() - offset
        )));
    }

    Ok(values)
}

/// Decode a 16-byte IP field. IPv4-mapped IPv6 addresses (::ffff:x.x.x.x)
/// are returned as Ipv4Addr.
fn decode_ip(bytes: [u8; 16]) -> IpAddr {
    let addr = Ipv6Addr::from(bytes);
    if let Some(v4) = addr.to_ipv4_mapped() {
        IpAddr::V4(v4)
    } else {
        IpAddr::V6(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn decode_ipv4_mapped() {
        let mut bytes = [0u8; 16];
        bytes[10] = 0xff;
        bytes[11] = 0xff;
        bytes[12] = 192;
        bytes[13] = 168;
        bytes[14] = 1;
        bytes[15] = 1;
        assert_eq!(decode_ip(bytes), IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)));
    }

    #[test]
    fn decode_ipv6() {
        let addr = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
        let bytes: [u8; 16] = addr.octets();
        assert_eq!(decode_ip(bytes), IpAddr::V6(addr));
    }

    #[test]
    fn decode_key_multiple_fields() {
        let fields = vec![FieldId::SrcIp, FieldId::IpProto, FieldId::Dscp];
        let mut key = vec![0u8; 18];
        key[10] = 0xff;
        key[11] = 0xff;
        key[12] = 10;
        key[15] = 1;
        key[16] = 6;
        key[17] = 46;

        let values = decode_key(&fields, &key).unwrap();
        assert_eq!(values.len(), 3);
        assert_eq!(
            values[0],
            FieldValue::Ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)))
        );
        assert_eq!(values[1], FieldValue::Proto(6));
        assert_eq!(values[2], FieldValue::Dscp(46));
    }

    #[test]
    fn decode_key_too_short() {
        let fields = vec![FieldId::SrcIp];
        let key = vec![0u8; 10];
        assert!(decode_key(&fields, &key).is_err());
    }

    #[test]
    fn decode_key_trailing_bytes() {
        let fields = vec![FieldId::IpProto];
        let key = vec![6, 0]; // 1 byte for proto + 1 trailing byte
        let err = decode_key(&fields, &key).unwrap_err();
        assert!(err.to_string().contains("trailing bytes"));
    }
}
