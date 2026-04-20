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
