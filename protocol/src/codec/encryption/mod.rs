pub mod game;
pub mod login;
pub mod noop;

use bytes::BytesMut;

use crate::protocol::{Protocol, Role};

/// Stateful stream encryptor.
/// Encrypts plaintext bytes and appends ciphertext to `dst`.
pub trait Encryptor: Send + std::fmt::Debug {
    fn encrypt(&mut self, src: &[u8], dst: &mut BytesMut);
}

/// Stateful stream decryptor.
/// Decrypts ciphertext bytes and appends plaintext to `dst`.
pub trait Decryptor: Send + std::fmt::Debug {
    fn decrypt(&mut self, src: &[u8], dst: &mut BytesMut);
}

pub fn no_encryption_encryptor() -> Box<dyn Encryptor> {
    Box::new(noop::NoopEncryptor)
}

pub fn no_encryption_decryptor() -> Box<dyn Decryptor> {
    Box::new(noop::NoopDecryptor)
}

pub fn cipher_pair(protocol: &Protocol, role: Role) -> (Box<dyn Encryptor>, Box<dyn Decryptor>) {
    match (protocol, role) {
        (Protocol::Login(info), Role::Server) => login::server_pair(info),
        (Protocol::Login(info), Role::Client) => login::client_pair(info),
        (Protocol::Game(info), Role::Server) => game::server_pair(info),
        (Protocol::Game(info), Role::Client) => game::client_pair(info),
    }
}
