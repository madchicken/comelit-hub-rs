use crate::command::Command;

pub struct UDPMChannel {
    control: [u8; 2],
    counter: [u8; 2],
}

impl UDPMChannel {
    pub fn new(control: &[u8; 2]) -> Self {
        UDPMChannel {
            control: *control,
            counter: [0, 1],
        }
    }

    pub fn open(&self) -> Vec<u8> {
        Command::channel(&String::from("UDPM"), &self.control, None, Some(1))
    }

    pub fn close(&self) -> Vec<u8> {
        Command::close(&self.control)
    }

    pub fn ping(&mut self, id: &[u8]) -> Vec<u8> {
        self.counter[1] += 1;
        let req = [id, &self.counter, &[0x00, 0x80]].concat();
        Command::make(&req, &self.control)
    }
}
