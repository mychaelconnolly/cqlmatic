#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};
use std::fmt::Write;

pub mod app;
pub mod assess;
pub mod catalog;
pub mod contracts;
pub mod discovery;
pub mod environment;
pub mod inventory;
pub mod language;
pub mod mcp;
pub mod network;
pub mod setup;
pub mod storage;

/// SHA-256 encoded as 64 lowercase hexadecimal digits for persisted fingerprints.
pub fn sha256_hex(input: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(input) {
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hex
}

#[cfg(test)]
mod schemas;

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn sha256_hex_preserves_known_digests_and_leading_zeros() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"286"),
            "00328ce57bbc14b33bd6695bc8eb32cdf2fb5f3a7d89ec14a42825e15d39df60"
        );
        assert_eq!(
            sha256_hex(&[0, 1, 15, 16, 255]),
            "6dc275912e5ad714d968f2c8ab33c9035eb0d2c59e4b60525df3c2bbab878334"
        );
    }
}
