use crate::command::Command;

const OPEN_STREAM_TEMPLATE: [u8; 8] = [0x00, 0x00, 0x3A, 0x12, 0x00, 0x67, 0x00, 0x80];

pub struct RTSPChannel {
    control: [u8; 2],
}

impl RTSPChannel {
    pub fn new(control: &[u8; 2]) -> Self {
        RTSPChannel { control: *control }
    }

    pub fn open(&self, sub: &String) -> Vec<u8> {
        Command::channel(&String::from("RTSP"), &self.control, Some(sub.as_bytes()))
    }

    pub fn close(&self) -> Vec<u8> {
        Command::close(&self.control)
    }

    pub fn open_stream(&self) -> Vec<u8> {
        let req = [&OPEN_STREAM_TEMPLATE[..]].concat();
        Command::make(&req, &self.control)
    }

    fn set_bytes(template: &mut [u8], bytes: &[u8], offset: usize) {
        for (i, b) in bytes.iter().enumerate() {
            template[i + offset] = *b;
        }
    }
}
