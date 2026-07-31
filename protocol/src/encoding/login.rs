use bytes::{BufMut, BytesMut};
use u_core::ProtocolVersion;

#[derive(Debug)]
pub struct XorLoginCrypt {
    client_key1: u32,
    client_key2: u32,
    client_key3: u32,

    key0: u32,
    key1: u32,
}

impl XorLoginCrypt {
    pub fn new(seed: u32, version: ProtocolVersion) -> XorLoginCrypt {
        let mut result = XorLoginCrypt {
            client_key1: 0,
            client_key2: 0,
            client_key3: 0,

            key0: 0,
            key1: 0,
        };

        result.set_seed(seed);
        result.calculate_client_key(version);

        result
    }

    pub fn set_seed(&mut self, seed: u32) {
        self.key0 = ((!seed ^ 0x00001357) << 16) | ((seed ^ 0xFFFFAAAA) & 0x0000FFFF);
        self.key1 = ((seed ^ 0x43210000) >> 16) | ((!seed ^ 0xABCDFFFF) & 0xFFFF0000);
    }

    fn set_client_keys(&mut self, key1: u32, key2: u32, key3: u32) {
        self.client_key1 = key1;
        self.client_key2 = key2;
        self.client_key3 = key3;
    }

    pub fn calculate_client_key(&mut self, version: ProtocolVersion) {
        let major = version.major;
        let minor = version.minor;
        let revision = version.patch;

        let mut key1 = (major << 23) | (minor << 14) | (revision << 4);
        key1 ^= (revision * revision) << 9;
        key1 ^= minor * minor;
        key1 ^= (minor * 11) << 24;
        key1 ^= (revision * 7) << 19;
        key1 ^= 0x2C13A5FD;

        let mut key2 = (major << 22) | (minor << 3) | (revision << 13);
        key2 ^= (revision * revision * 3) << 10;
        key2 ^= minor * minor;
        key2 ^= (minor * 13) << 23;
        key2 ^= (revision * 7) << 18;
        key2 ^= 0xA31D527F;

        self.set_client_keys(key1, key2, 0);
    }

    fn next_keys(&mut self) {
        let old_key0 = self.key0;
        let old_key1 = self.key1;

        self.key0 = ((old_key0 >> 1) | (old_key1 << 31)) ^ self.client_key2;
        self.key1 = (((((old_key1 >> 1) | (old_key0 << 31)) ^ self.client_key1) >> 1)
            | (old_key0 << 31))
            ^ self.client_key1;
    }

    pub fn encode(&mut self, src: &[u8], dst: &mut BytesMut) {
        dst.reserve(src.len());
        for &x in src {
            dst.put_u8(x ^ self.key0 as u8);
            self.next_keys();
        }
    }
}
