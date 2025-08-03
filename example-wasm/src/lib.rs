static mut COUNTER: i32 = 0;

#[no_mangle]
pub extern "C" fn run() -> i32 {
    42
}

#[no_mangle]
pub extern "C" fn increment() -> i32 {
    unsafe {
        COUNTER += 1;
        COUNTER
    }
}

#[no_mangle]
pub extern "C" fn crash() -> i32 {
    let ptr = 0x0FFFFFFFusize as *mut u8;
    unsafe {
        *ptr = 1;
    }
    0
}
