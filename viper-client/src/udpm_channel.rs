use crate::{command::Command, helper::Helper};
pub struct UDPMChannel {
    control: [u8; 2],
}

impl UDPMChannel {
    pub fn new(control: &[u8; 2]) -> Self {
        UDPMChannel { control: *control }
    }

    pub fn open(&self) -> Vec<u8> {
        Command::channel(&String::from("UDPM"), &self.control, None, Some(1))
    }

    pub fn close(&self) -> Vec<u8> {
        Command::close(&self.control)
    }

    pub fn init_main(&self, id: &[u8]) -> Vec<u8> {
        let mut req = [id, &[0x00, 0xF7, 0x00, 0x80]].concat();

        Helper::pad(&mut req);
        Command::make(&req, &self.control)
    }

    pub fn init_video(&self, id: &[u8]) -> Vec<u8> {
        let mut req = [id, &[0x00, 0x67, 0x00, 0x80]].concat();

        Helper::pad(&mut req);
        Command::make(&req, &self.control)
    }

    pub fn init_audio_in(&self, id: &[u8]) -> Vec<u8> {
        let mut req = [id, &[0x00, 0x68, 0x00, 0x80]].concat();

        Helper::pad(&mut req);
        Command::make(&req, &self.control)
    }

    pub fn init_audio_out(&self, id: &[u8]) -> Vec<u8> {
        let mut req = [id, &[0x00, 0x69, 0x00, 0x80]].concat();

        Helper::pad(&mut req);
        Command::make(&req, &self.control)
    }
}
