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
        let mut iter = buffer.iter().peekable();

        let mut rows = vec![];
        while iter.peek().is_some() {
            let mut chunk = iter.by_ref().take(8);
            let mut hex = vec![];
            let mut chars = vec![];

            chunk.by_ref().for_each(|x| {
                hex.push(format!("{:02X}", x));
                chars.push(if x.is_ascii_graphic() {
                    format!("{}", *x as char)
                } else {
                    ".".to_string()
                });
            });

            if hex.len() < 8 {
                hex.extend(vec!["  ".to_string(); 8 - hex.len()]);
                chars.extend(vec![" ".to_string(); 8 - chars.len()]);
            }

            rows.push(format!("{} |{}|", hex.join(" "), chars.join(" ")));
        }
        debug!("\n{}", rows.join("\n"));
    }
}
