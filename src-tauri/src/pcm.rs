//! Continuous 24 kHz mono PCM playback for the local speech engines.
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;

pub const SAMPLE_RATE: usize = 24_000;
const BUFFER_FRAMES: usize = 512;
const BUFFER_COUNT: usize = 3;

#[derive(Default)]
struct SampleBuffer {
    samples: VecDeque<f32>,
    finished: bool,
    cancelled: bool,
    underrun_frames: usize,
    draining: bool,
    error: i32,
}
impl SampleBuffer {
    fn read(&mut self, output: &mut [f32]) -> usize {
        if self.cancelled {
            self.samples.clear();
            return 0;
        }
        let copied = output.len().min(self.samples.len());
        for sample in &mut output[..copied] {
            *sample = self.samples.pop_front().expect("length checked");
        }
        if self.finished {
            return copied;
        }
        output[copied..].fill(0.0);
        self.underrun_frames += output.len() - copied;
        output.len()
    }
}

// Layouts and declarations follow AudioQueue.h and CoreAudioBaseTypes.h in the macOS SDK.
#[repr(C)]
struct StreamFormat {
    sample_rate: f64,
    format: u32,
    flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels: u32,
    bits: u32,
    reserved: u32,
}
#[repr(C)]
struct AudioBuffer {
    capacity: u32,
    data: *mut c_void,
    size: u32,
    user_data: *mut c_void,
    packet_capacity: u32,
    packets: *mut c_void,
    packet_count: u32,
}
type Queue = *mut c_void;
#[link(name = "AudioToolbox", kind = "framework")]
extern "C" {
    fn AudioQueueNewOutput(
        format: *const StreamFormat,
        callback: unsafe extern "C" fn(*mut c_void, Queue, *mut AudioBuffer),
        user: *mut c_void,
        run_loop: *const c_void,
        mode: *const c_void,
        flags: u32,
        queue: *mut Queue,
    ) -> i32;
    fn AudioQueueAllocateBuffer(queue: Queue, capacity: u32, buffer: *mut *mut AudioBuffer) -> i32;
    fn AudioQueueEnqueueBuffer(
        queue: Queue,
        buffer: *mut AudioBuffer,
        packets: u32,
        descriptions: *const c_void,
    ) -> i32;
    fn AudioQueueStart(queue: Queue, time: *const c_void) -> i32;
    fn AudioQueueStop(queue: Queue, immediate: u8) -> i32;
    fn AudioQueueDispose(queue: Queue, immediate: u8) -> i32;
    fn AudioQueueGetProperty(queue: Queue, property: u32, data: *mut c_void, size: *mut u32)
        -> i32;
}

unsafe extern "C" fn refill(user: *mut c_void, queue: Queue, buffer: *mut AudioBuffer) {
    // The boxed mutex outlives the queue; synchronous Dispose joins its callbacks.
    let shared = &*(user as *const Mutex<SampleBuffer>);
    let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
    if state.cancelled || state.draining {
        return;
    }
    let output = std::slice::from_raw_parts_mut((*buffer).data as *mut f32, BUFFER_FRAMES);
    let frames = state.read(output);
    let result = if frames == 0 {
        state.draining = true;
        drop(state);
        // Async stop drains already submitted audio. A refill callback does NOT mean
        // the last samples are audible yet; the owner waits for IsRunning to become 0.
        AudioQueueStop(queue, 0)
    } else {
        (*buffer).size = (frames * 4) as u32;
        drop(state);
        AudioQueueEnqueueBuffer(queue, buffer, 0, ptr::null())
    };
    if result != 0 {
        shared.lock().unwrap_or_else(|e| e.into_inner()).error = result;
    }
}

/// One native queue for an entire reading. Producer writes and callback reads only PCM;
/// no inference, file I/O or process launches occur in the callback.
pub struct Player {
    queue: Queue,
    state: Box<Mutex<SampleBuffer>>,
    buffers: [*mut AudioBuffer; BUFFER_COUNT],
    control: Mutex<bool>, // serialized start/stop; value means started
}
// AudioQueue supports calls from multiple threads. Start/stop/property operations are
// serialized with control; callbacks use only state, and never wait for control.
unsafe impl Send for Player {}
unsafe impl Sync for Player {}

fn check(status: i32) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("Core Audio playback failed ({status})"))
    }
}

impl Player {
    pub fn new() -> Result<Self, String> {
        let state = Box::new(Mutex::new(SampleBuffer::default()));
        let format = StreamFormat {
            sample_rate: SAMPLE_RATE as f64,
            format: u32::from_be_bytes(*b"lpcm"),
            flags: 1 | (1 << 3), // native-endian float, packed
            bytes_per_packet: 4,
            frames_per_packet: 1,
            bytes_per_frame: 4,
            channels: 1,
            bits: 32,
            reserved: 0,
        };
        let mut queue = ptr::null_mut();
        unsafe {
            check(AudioQueueNewOutput(
                &format,
                refill,
                &*state as *const Mutex<SampleBuffer> as *mut c_void,
                ptr::null(),
                ptr::null(),
                0,
                &mut queue,
            ))?;
        }
        let mut player = Self {
            queue,
            state,
            buffers: [ptr::null_mut(); BUFFER_COUNT],
            control: Mutex::new(false),
        };
        for buffer in &mut player.buffers {
            unsafe {
                check(AudioQueueAllocateBuffer(
                    queue,
                    (BUFFER_FRAMES * 4) as u32,
                    buffer,
                ))?;
            }
        }
        Ok(player)
    }

    pub fn push(&self, samples: Vec<f32>, last: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.cancelled {
            state.samples.extend(samples);
            state.finished = last;
        }
    }

    pub fn buffered_seconds(&self) -> f64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .samples
            .len() as f64
            / SAMPLE_RATE as f64
    }

    pub fn underrun_seconds(&self) -> f64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .underrun_frames as f64
            / SAMPLE_RATE as f64
    }

    pub fn start(&self) -> Result<(), String> {
        let mut started = self.control.lock().unwrap_or_else(|e| e.into_inner());
        if *started {
            return Ok(());
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.cancelled {
            return Ok(());
        }
        for buffer in self.buffers {
            unsafe {
                let output =
                    std::slice::from_raw_parts_mut((*buffer).data as *mut f32, BUFFER_FRAMES);
                let frames = state.read(output);
                if frames == 0 {
                    break;
                }
                (*buffer).size = (frames * 4) as u32;
                check(AudioQueueEnqueueBuffer(self.queue, buffer, 0, ptr::null()))?;
            }
        }
        drop(state);
        unsafe {
            check(AudioQueueStart(self.queue, ptr::null()))?;
        }
        *started = true;
        Ok(())
    }

    pub fn is_playing(&self) -> Result<bool, String> {
        let started = self.control.lock().unwrap_or_else(|e| e.into_inner());
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        check(state.error)?;
        if !*started || state.cancelled {
            return Ok(false);
        }
        if !state.draining {
            return Ok(true);
        }
        drop(state);
        let mut running = 0u32;
        let mut size = 4u32;
        unsafe {
            check(AudioQueueGetProperty(
                self.queue,
                u32::from_be_bytes(*b"aqrn"),
                &mut running as *mut u32 as *mut c_void,
                &mut size,
            ))?;
        }
        Ok(running != 0)
    }

    pub fn stop(&self) {
        let _control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.cancelled = true;
            state.samples.clear();
        }
        unsafe {
            AudioQueueStop(self.queue, 1);
        }
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        // Stop before disposing; state is freed only after no callbacks can reference it.
        self.stop();
        unsafe {
            AudioQueueDispose(self.queue, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn samples_cross_generation_boundaries_without_gaps() {
        let mut buffer = SampleBuffer::default();
        buffer.samples.extend([0.1, 0.2]);
        buffer.samples.extend([0.3, 0.4]);
        buffer.finished = true;
        let mut out = [0.; 8];
        assert_eq!(buffer.read(&mut out), 4);
        assert_eq!(&out[..4], &[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(buffer.read(&mut out), 0);
        assert_eq!(buffer.underrun_frames, 0);
    }
    #[test]
    fn starvation_keeps_output_alive_and_counts_only_missing_samples() {
        let mut buffer = SampleBuffer::default();
        buffer.samples.push_back(0.5);
        let mut out = [9.; 4];
        assert_eq!(buffer.read(&mut out), 4);
        assert_eq!(out, [0.5, 0., 0., 0.]);
        assert_eq!(buffer.underrun_frames, 3);
        buffer.samples.extend([0.6, 0.7]);
        buffer.finished = true;
        assert_eq!(buffer.read(&mut out), 2);
        assert_eq!(&out[..2], &[0.6, 0.7]);
    }
    #[test]
    fn cancellation_drops_buffered_audio() {
        let mut buffer = SampleBuffer::default();
        buffer.samples.extend([1.; 20]);
        buffer.cancelled = true;
        assert_eq!(buffer.read(&mut [0.; 4]), 0);
        assert!(buffer.samples.is_empty());
    }
}
