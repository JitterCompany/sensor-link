#![allow(unused)] // TODO REMOVE, this file is still WIP
use core::ops::Range;

use super::{block_layer, Object, ObjectId};

#[derive(PartialEq, Debug)]
#[repr(u32)]
pub enum Mode {
    WriteOnce,
    Circular,
}

pub const OBJECT_HEADER_SIZE: usize = 20;
#[derive(Debug)]
pub struct ObjectHeader {
    id: u16,
    frag_size: u16,
    first_block: u32,
    n_blocks: u32,
    file_size: u32,
    mode: Mode,
}

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum FAT {
    Root,
}

impl<const BLOCK_SIZE: usize> Object<BLOCK_SIZE> for FAT {
    fn id(&self) -> ObjectId {
        self.clone() as ObjectId
    }

    fn flash_blocks(&self) -> Range<block_layer::BlockId> {
        0..1
    }

    fn fragment_size(&self) -> usize {
        OBJECT_HEADER_SIZE
    }
}
