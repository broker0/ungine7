use crate::encoding::huffman_tables::{COMPRESS_TABLE, DECOMPRESS_TREE};
use bytes::{BufMut, BytesMut};

const BITS_IN_BYTE: usize = 8;
const END_OF_STREAM_CODE: i32 = 256;

#[derive(Debug)]
pub struct HuffmanDecompress {
    // bit: usize,
    tree_pos: i32,
    // mask: u8,
    // data: u8,
}

impl HuffmanDecompress {
    pub fn new() -> Self {
        Self {
            // bit: BITS_IN_BYTE,
            tree_pos: 0,
            // mask: 0,
            // data: 0,
        }
    }

    pub fn decompress(&mut self, src: &[u8], dst: &mut BytesMut) {
        dst.reserve(src.len());

        for &byte in src {
            for mask in [0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01] {
                self.tree_pos = match byte & mask != 0 {
                    true => DECOMPRESS_TREE[self.tree_pos as usize].0, // left child
                    false => DECOMPRESS_TREE[self.tree_pos as usize].1, // right child
                };

                if self.tree_pos <= 0 {
                    // leaf node
                    let code = -self.tree_pos; // decoded value
                    self.tree_pos = 0;

                    if code == END_OF_STREAM_CODE {
                        // special flush code
                        break; // ignore rest of byte, no data, goto next
                    }

                    dst.put_u8(code as u8); // append decoded byte
                }
            }
        }
    }

    /*pub fn decompress2(&mut self, src: &[u8], dst: &mut BytesMut) {
        let mut bytes = src.iter();

        loop {
            if self.bit >= BITS_IN_BYTE {  // fetch next byte
                let next_byte = match bytes.next() {
                    Some(v) => *v,
                    None => break,  // no more data
                };

                self.data = next_byte;
                self.bit = 0;
                self.mask = 0x80;
            }

            self.tree_pos = match self.data & self.mask != 0 {
                true  => DECOMPRESS_TREE[self.tree_pos as usize].0,
                false => DECOMPRESS_TREE[self.tree_pos as usize].1,
            };

            self.mask >>= 1;
            self.bit += 1;

            if self.tree_pos <= 0 {  // leaf node, value
                let code = -self.tree_pos;   // decoded value
                self.tree_pos = 0;

                if code == END_OF_STREAM_CODE {   // special flush code
                    self.bit = BITS_IN_BYTE; // ignore rest of byte, no data, goto next
                    continue;
                }

                dst.put_u8(code as u8);   // append decoded byte
            }
        }
    }*/
}

#[derive(Debug)]
pub struct HuffmanCompress {
    data: i32,
    len: usize,
}

impl HuffmanCompress {
    pub fn new() -> Self {
        Self { data: 0, len: 0 }
    }

    pub fn compress(&mut self, src: &[u8], dst: &mut BytesMut) {
        dst.reserve(src.len());

        for &byte in src {
            self.write_bits(byte as i32, dst);
        }

        self.write_bits(END_OF_STREAM_CODE, dst);
    }

    #[inline]
    fn write_bits(&mut self, code: i32, dst: &mut BytesMut) {
        let (code_len, code_val) = COMPRESS_TABLE[code as usize];

        self.data = (self.data << code_len) | code_val;
        self.len += code_len;

        self.flush(dst, code == END_OF_STREAM_CODE);
    }

    #[inline]
    fn flush(&mut self, dst: &mut BytesMut, force: bool) {
        while self.len >= BITS_IN_BYTE {
            // output full-filled bytes from buffer
            self.len -= BITS_IN_BYTE;
            dst.put_u8((self.data >> self.len) as u8);
        }

        if force && self.len > 0 {
            // if flush is forced, then output non full-filled byte
            self.data <<= BITS_IN_BYTE - self.len;
            self.len = 0;
            dst.put_u8(self.data as u8);
        }
    }
}
