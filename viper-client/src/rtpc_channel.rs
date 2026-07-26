use crate::{
    command::Command,
    command_response::{Opendoor, VipConfig},
    helper::{Helper, NULL},
};

const START_STREAM_TEMPLATE: [u8; 10] = [
    0xC0, 0x18, 0x1A, 0x41, //
    0xA8, 0xC8, 0x00, 0x28, //
    0x00, 0x01, //
];

pub struct RTPCChannel {
    control: [u8; 2],
}

impl RTPCChannel {
    pub fn new(control: &[u8; 2]) -> Self {
        RTPCChannel { control: *control }
    }

    pub fn open(&self) -> Vec<u8> {
        Command::channel(&String::from("RTPC"), &self.control, None, Some(1))
    }

    pub fn close(&self) -> Vec<u8> {
        Command::close(&self.control)
    }

    pub fn start_stream(&self, vip: &VipConfig) -> Vec<u8> {
        let apt_combined = format!("{}{}", vip.apt_address, vip.apt_subaddress);
        let mut req = [
            START_STREAM_TEMPLATE[..].to_vec(),
            self.control[..].to_vec(),
            vec![
                0x00, 0x00, 0xff, 0xff, 0xff, 0xff, //
            ],
            Helper::string_to_buffer(apt_combined.as_str(), true),
            NULL.to_vec(),
        ]
        .concat();
        Helper::pad(&mut req);
        Command::make(&req, &self.control)
    }
}
