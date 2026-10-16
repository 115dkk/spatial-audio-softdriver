#![allow(unsafe_code)]
use plughost_sys::vst2::*;
use std::ffi::c_void;

#[repr(C)]
struct Instance {
    effect: AEffect,
    host: Callback,
    chunk: [u8; 2],
    width: f32,
    delay: [[f32; 64]; 6],
    cursor: usize,
    active: bool,
    input: Arrangement,
    output: Arrangement,
    rect: Rect,
}

// SAFETY: the entry symbol has the VST 2 ABI, has no competing definition, and
// returns an owned instance whose lifetime is ended by CLOSE.
#[unsafe(no_mangle)]
pub extern "C" fn VSTPluginMain(host: Callback) -> *mut AEffect {
    let instance = Box::new(Instance {
        effect: AEffect {
            dispatcher: Some(dispatch),
            set_parameter: Some(set_parameter),
            get_parameter: Some(get_parameter),
            num_programs: 1,
            num_params: 1,
            num_inputs: 2,
            num_outputs: 6,
            flags: HAS_EDITOR | REPLACING | PROGRAM_CHUNKS,
            initial_delay: 64,
            unique_id: i32::from_be_bytes(*b"SSTP"),
            version: 1,
            process_replacing: Some(process),
            ..AEffect::default()
        },
        host,
        chunk: [1, 0],
        width: 1.0,
        delay: [[0.0; 64]; 6],
        cursor: 0,
        active: false,
        input: Arrangement::new(ARR_STEREO, &[1, 2]),
        output: Arrangement::new(ARR_51, &[1, 2, 3, 4, 5, 6]),
        rect: Rect {
            top: 0,
            left: 0,
            bottom: 240,
            right: 320,
        },
    });
    Box::into_raw(instance).cast()
}

unsafe extern "C" fn dispatch(
    effect: *mut AEffect,
    opcode: i32,
    _index: i32,
    value: isize,
    pointer: *mut c_void,
    _option: f32,
) -> isize {
    if effect.is_null() {
        return 0;
    }
    if opcode == CLOSE {
        // SAFETY: CLOSE is called exactly once for the Box allocated by VSTPluginMain.
        drop(unsafe { Box::from_raw(effect.cast::<Instance>()) });
        return 1;
    }
    // SAFETY: host serializes dispatch/state with processing and effect points to our instance.
    let instance = unsafe { &mut *effect.cast::<Instance>() };
    match opcode {
        OPEN | SET_SAMPLE_RATE | SET_BLOCK_SIZE | START_PROCESS | STOP_PROCESS | EDIT_OPEN
        | EDIT_CLOSE | EDIT_IDLE | SET_PROGRAM => 1,
        GET_PROGRAM => 0,
        MAINS_CHANGED => {
            instance.active = value != 0;
            1
        }
        GET_EFFECT_NAME | GET_VENDOR_STRING | PARAM_NAME => {
            let text: &[u8] = match opcode {
                GET_EFFECT_NAME => b"Test matrix\0",
                GET_VENDOR_STRING => b"Test fixtures\0",
                _ => b"Width\0",
            };
            if pointer.is_null() {
                return 0;
            }
            // SAFETY: these opcodes supply a writable string buffer of at least 24 bytes.
            unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), pointer.cast(), text.len()) };
            1
        }
        GET_SPEAKER_ARRANGEMENT => {
            if value == 0 || pointer.is_null() {
                return 0;
            }
            // SAFETY: opcode supplies an input arrangement pointer-out in value.
            unsafe { *(value as *mut *mut Arrangement) = &mut instance.input };
            // SAFETY: opcode supplies an output arrangement pointer-out in pointer.
            unsafe { *pointer.cast::<*mut Arrangement>() = &mut instance.output };
            1
        }
        SET_SPEAKER_ARRANGEMENT => 1,
        GET_CHUNK => {
            if pointer.is_null() {
                return 0;
            }
            // SAFETY: GET_CHUNK supplies a pointer-out; chunk remains live until next state call.
            unsafe { *pointer.cast::<*const u8>() = instance.chunk.as_ptr() };
            2
        }
        SET_CHUNK => {
            if value != 2 || pointer.is_null() {
                return 0;
            }
            // SAFETY: SET_CHUNK supplies value readable bytes for this synchronous call.
            let chunk = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), 2) };
            if chunk[0] != 1 || chunk[1] > 4 {
                return 0;
            }
            if chunk[1] == 4 {
                std::process::abort();
            }
            instance.chunk.copy_from_slice(chunk);
            1
        }
        EDIT_GET_RECT => {
            if pointer.is_null() {
                return 0;
            }
            // SAFETY: EDIT_GET_RECT supplies a rectangle pointer-out.
            unsafe { *pointer.cast::<*mut Rect>() = &mut instance.rect };
            1
        }
        _ => 0,
    }
}
unsafe extern "C" fn set_parameter(effect: *mut AEffect, index: u32, value: f32) {
    if effect.is_null() || index != 0 {
        return;
    }
    // SAFETY: host serializes parameter changes with processing; effect is our live instance.
    let instance = unsafe { &mut *effect.cast::<Instance>() };
    instance.width = value.clamp(0.0, 1.0);
    let host = instance.host;
    // SAFETY: callback belongs to the host that created this instance; no pointer payload.
    unsafe { host(effect, MASTER_AUTOMATE, 0, 0, std::ptr::null_mut(), value) };
}
unsafe extern "C" fn get_parameter(effect: *mut AEffect, index: u32) -> f32 {
    if effect.is_null() || index != 0 {
        return 0.0;
    }
    // SAFETY: host serializes parameter access; effect is our live instance.
    unsafe { (*effect.cast::<Instance>()).width }
}
unsafe extern "C" fn process(
    effect: *mut AEffect,
    inputs: *const *const f32,
    outputs: *mut *mut f32,
    frames: i32,
) {
    if effect.is_null() || inputs.is_null() || outputs.is_null() || frames <= 0 {
        return;
    }
    // SAFETY: processing has exclusive ownership of the live instance until return.
    let instance = unsafe { &mut *effect.cast::<Instance>() };
    if !instance.active {
        return;
    }
    match instance.chunk[1] {
        1 => std::process::abort(),
        2 => loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        },
        _ => {}
    }
    // SAFETY: host provides two input channel pointers.
    let inputs = unsafe { std::slice::from_raw_parts(inputs, 2) };
    // SAFETY: host provides six output channel pointers.
    let outputs = unsafe { std::slice::from_raw_parts(outputs, 6) };
    for frame in 0..frames as usize {
        // SAFETY: first input has frames readable samples; read before in-place writes.
        let left = unsafe { *inputs[0].wrapping_add(frame) };
        // SAFETY: second input has frames readable samples; read before in-place writes.
        let right = unsafe { *inputs[1].wrapping_add(frame) };
        let center = 0.5 * (left + right);
        let values = [
            left,
            right,
            center,
            center,
            0.5 * left * instance.width,
            0.5 * right * instance.width,
        ];
        for channel in 0..6 {
            let delayed = instance.delay[channel][instance.cursor];
            instance.delay[channel][instance.cursor] = values[channel];
            let sample = if instance.chunk[1] == 3 && channel == 2 {
                f32::NAN
            } else {
                delayed
            };
            // SAFETY: each output has frames writable samples, including in-place input aliases.
            unsafe { *outputs[channel].wrapping_add(frame) = sample };
        }
        instance.cursor = (instance.cursor + 1) % 64;
    }
}
