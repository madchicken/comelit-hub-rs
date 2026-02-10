use rand::Rng;
use tracing::debug;

const START: u8 = 0x01;

// The reason why it stops at 0x80 is because in some parts of the
// protocol the server adds 0x80 to the total, which could result in
// a number higher than 0xff, which doesn't exist.
const END: u8 = 0x80;
pub const NULL: &[u8] = &[0x00];

pub struct Helper {}

#[allow(dead_code)]
impl Helper {
    pub fn gen_ran(size: usize) -> Vec<u8> {
        let mut rng = rand::rng();

        (0..size)
            .map(|_| rng.random_range(START..END))
            .collect::<Vec<u8>>()
    }

    pub fn control() -> [u8; 2] {
        let mut rng = rand::rng();
        [rng.random_range(START..END), rng.random_range(START..END)]
    }

    // Helper function to convert a string to a buffer with optional null termination
    pub fn string_to_buffer(s: &str, null_terminated: bool) -> Vec<u8> {
        let mut buffer = s.as_bytes().to_vec();
        if null_terminated {
            buffer.push(0x00);
        }
        buffer
    }

    pub fn pad(buffer: &mut Vec<u8>) {
        if !buffer.len().is_multiple_of(2) {
            buffer.extend(NULL);
        }
    }

    pub fn print_buffer(buffer: &[u8]) {
        debug!(
            "{}",
            buffer
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    let byte = format!("{:02X}", x);
                    let terminator = if i.is_multiple_of(8) {
                        String::from("\n")
                    } else {
                        String::from(" ")
                    };
                    format!("{terminator}{byte}")
                })
                .collect::<Vec<String>>()
                .join("")
        );
    }
}
