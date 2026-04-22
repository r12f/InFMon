use std::net::{IpAddr, Ipv6Addr};

use super::error::IpcError;
use super::types::*;

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
            FieldId::SrcPort | FieldId::DstPort => {
                if offset + 2 > key_bytes.len() {
                    return Err(IpcError::StatsFormat(format!(
                        "key too short for {:?} at offset {}: need 2 bytes, have {}",
                        field,
                        offset,
                        key_bytes.len() - offset
                    )));
                }
                let port = u16::from_be_bytes([key_bytes[offset], key_bytes[offset + 1]]);
                values.push(FieldValue::Port(port));
                offset += 2;
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
#[path = "decode_tests.rs"]
mod decode_tests;
