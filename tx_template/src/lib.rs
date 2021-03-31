// TODO the memory types, serialization, and other "plumbing" code will be
// injected into the wasm module by the host to reduce file size
use anoma_vm_env::memory;
use borsh::{BorshDeserialize, BorshSerialize};
use core::slice;
use std::mem::size_of;

/// The environment provides calls to host functions via this C interface:
extern "C" {
    fn read(key_ptr: u64, key_len: u64, result_ptr: u64) -> u64;
    fn write(key_ptr: u64, key_len: u64, val_ptr: u64, val_len: u64) -> u64;
    fn delete(key_ptr: u64, key_len: u64) -> u64;
    // Requires a node running with "Info" log level
    fn log_string(str_ptr: u64, str_len: u64);
    // fn iterate_prefix(key) -> iter;
    // fn iter_next(iter) -> (key, value);
}

/// The module interface callable by wasm runtime:
#[no_mangle]
pub extern "C" fn apply_tx(tx_data_ptr: u64, tx_data_len: u64) {
    let slice = unsafe { slice::from_raw_parts(tx_data_ptr as *const u8, tx_data_len as _) };
    let tx_data = slice.to_vec() as memory::TxData;

    let log_msg = format!("apply_tx called with tx_data: {:#?}", tx_data);
    unsafe {
        log_string(log_msg.as_ptr() as _, log_msg.len() as _);
    }

    do_apply_tx(tx_data);
}

fn do_apply_tx(_tx_data: memory::TxData) {
    // source and destination address
    let src_key = "va/balance/eth";
    let dest_key = "ba/balance/eth";
    let amount = 10;

    let bal_size = size_of::<u64>();
    let src_bal_buf: Vec<u8> = Vec::with_capacity(bal_size);
    let result = unsafe {
        read(
            src_key.as_ptr() as _,
            src_key.len() as _,
            src_bal_buf.as_ptr() as _,
        )
    };
    if result == 1 {
        let mut slice = unsafe { slice::from_raw_parts(src_bal_buf.as_ptr(), bal_size) };
        let src_bal: u64 = u64::deserialize(&mut slice).unwrap();

        let dest_bal_buf: Vec<u8> = Vec::with_capacity(bal_size);
        let result = unsafe {
            read(
                dest_key.as_ptr() as _,
                dest_key.len() as _,
                dest_bal_buf.as_ptr() as _,
            )
        };
        if result == 1 {
            let mut slice = unsafe { slice::from_raw_parts(dest_bal_buf.as_ptr(), bal_size) };
            let dest_bal: u64 = u64::deserialize(&mut slice).unwrap();
            // TODO this doesn't work: runtime error
            // let dest_bal: u64 = u64::deserialize(&mut &dest_bal_buf[..]).unwrap();

            let src_new_bal = src_bal - amount;
            let dest_new_bal = dest_bal + amount;
            let mut src_new_bal_buf: Vec<u8> = Vec::with_capacity(8);
            src_new_bal.serialize(&mut src_new_bal_buf).unwrap();
            let mut dest_new_bal_buf: Vec<u8> = Vec::with_capacity(8);
            dest_new_bal.serialize(&mut dest_new_bal_buf).unwrap();

            unsafe {
                write(
                    src_key.as_ptr() as _,
                    src_key.len() as _,
                    src_new_bal_buf.as_ptr() as _,
                    src_new_bal_buf.len() as _,
                )
            };
            unsafe {
                write(
                    dest_key.as_ptr() as _,
                    dest_key.len() as _,
                    dest_new_bal_buf.as_ptr() as _,
                    dest_new_bal_buf.len() as _,
                )
            };
        }
    }
}