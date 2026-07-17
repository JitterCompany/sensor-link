use std::{
    io::{BufReader, Cursor},
    path::PathBuf,
};

use rustls_pemfile::Item;
use x509_parser::{certificate::X509Certificate, oid_registry::asn1_rs::FromDer};

use super::file::{find_file, read_file};

/// Validates cert and returns (Common Name, Cert content) if successful
pub fn parse_cert(path: &PathBuf) -> Option<(String, String)> {
    if let Some(cert_path) = find_file(path) {
        let cert_data = read_file(&cert_path);
        let mut cert_buf = BufReader::new(Cursor::new(cert_data.clone()));
        match rustls_pemfile::read_one(&mut cert_buf) {
            Ok(Some(Item::X509Certificate(cert))) => {
                match X509Certificate::from_der(cert.as_ref()) {
                    Ok((_re, cert)) => {
                        let cn = parse_common_name(cert).unwrap();
                        return Some((cn, cert_data));
                    }
                    Err(_) => panic!("Failed to parse cert file: {cert_path:?}"),
                }
            }
            _ => {
                panic!("Failed to parse cert file: {cert_path:?}");
            }
        }
    }
    println!("cargo:warning=No cert file found");
    None
}

pub fn parse_key(path: &PathBuf) -> Option<String> {
    if let Some(key_path) = find_file(path) {
        let key_data = read_file(&key_path);
        let mut key_buf = BufReader::new(Cursor::new(key_data.clone()));
        match rustls_pemfile::read_one(&mut key_buf).expect("Parsing key") {
            Some(Item::Sec1Key(_)) | Some(Item::Pkcs8Key(_)) | Some(Item::Pkcs1Key(_)) => {
                return Some(key_data);
            }
            _ => {
                panic!("Failed to parse key file: {key_path:?}");
            }
        }
    }
    None
}

pub fn parse_key_bytes(key_data: &[u8]) -> Option<String> {
    let mut key_buf = BufReader::new(Cursor::new(key_data.to_vec()));
    match rustls_pemfile::read_one(&mut key_buf).expect("Parsing key") {
        Some(Item::Sec1Key(_)) | Some(Item::Pkcs8Key(_)) | Some(Item::Pkcs1Key(_)) => {
            Some(String::from_utf8(key_data.to_vec()).expect("Key PEM must be UTF-8"))
        }
        _ => panic!("Failed to parse key bytes"),
    }
}

fn parse_common_name(cert: X509Certificate<'_>) -> Option<String> {
    let mut iter = cert.subject().iter_common_name();
    if let Some(cn) = iter.next() {
        if iter.next().is_none() {
            return Some(cn.as_str().expect("Parse CN to utf8 String").to_string());
        }
    }
    None
}
