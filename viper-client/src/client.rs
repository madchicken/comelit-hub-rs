use std::time::Duration;

use crate::{
    JSONResult, ViperError,
    audio_video::{Args, read_av_stream},
    channel::Channel,
    command::{CommandKind, PushInfo},
    command_response::{
        ActivateUserResponse, AuthResponse, ConfigurationResponse, InfoResponse, Opendoor,
        VipConfig,
    },
    ctpp_channel::CTPPChannel,
    helper::Helper,
    rtpc_channel::RTPCChannel,
    stream_wrapper::StreamWrapper,
    udp_stream_wrapper::UdpStreamWrapper,
    udpm_channel::UDPMChannel,
};
use serde::Deserialize;
use tokio::time::{Sleep, sleep};
use tracing::debug;

pub const ICONA_BRIDGE_PORT: u16 = 64100;

pub struct ViperClient {
    stream: StreamWrapper,
    udp: UdpStreamWrapper,
    control: [u8; 2],
}

impl ViperClient {
    pub async fn scan() -> Option<(String, u16)> {
        let scanner = comelit_client_rs::Scanner::scan(Some(Duration::from_secs(2))).await;
        if let Ok(devices) = scanner {
            devices
                .iter()
                .find(|d| d.app_id() == "HSrv" && d.address().is_some())
                .map(|d| {
                    let mut x = d.address().unwrap().split(":");
                    let ip = x.next().unwrap();
                    let port = x
                        .next()
                        .unwrap_or(ICONA_BRIDGE_PORT.to_string().as_str())
                        .parse()
                        .unwrap();
                    (ip.to_string(), port)
                })
        } else {
            None
        }
    }

    pub async fn new(ip: &str, port: u16) -> ViperClient {
        let doorbell = format!("{}:{}", ip, port);

        ViperClient {
            stream: StreamWrapper::new(doorbell),
            udp: UdpStreamWrapper::new(ip, port)
                .await
                .expect("Can't opend UDP connection"),
            control: Helper::control(),
        }
    }

    pub fn sign_up(&mut self, email: &str) -> JSONResult<ActivateUserResponse> {
        let fact_channel = self.channel("FACT");
        self.stream.execute(&fact_channel.open())?;
        let activate_user = CommandKind::ActivateUser(String::from(email));
        let act_bytes = self.stream.execute(&fact_channel.com(activate_user))?;
        let json_response = Self::json(&act_bytes);

        self.stream.execute(&fact_channel.close())?;
        json_response
    }

    pub fn remove_all_users(&mut self, email: &String) -> JSONResult<serde_json::Value> {
        let fact_channel = self.channel("FACT");
        self.stream.execute(&fact_channel.open())?;
        let remove_all_users = CommandKind::RemoveAllUsers(String::from(email));
        let rem_bytes = self.stream.execute(&fact_channel.com(remove_all_users))?;
        self.stream.execute(&fact_channel.close())?;

        Self::json(&rem_bytes)
    }

    pub fn authorize(&mut self, token: &str) -> JSONResult<AuthResponse> {
        let uaut = CommandKind::UAUT(token.into());
        let uaut_channel = self.channel("UAUT");
        self.stream.execute(&uaut_channel.open())?;
        let uaut_bytes = self.stream.execute(&uaut_channel.com(uaut))?;

        let json_response = Self::json(&uaut_bytes);
        self.stream.execute(&uaut_channel.close())?;
        json_response
    }

    pub fn configuration(&mut self, addressbooks: &str) -> JSONResult<ConfigurationResponse> {
        let ucfg = CommandKind::UCFG(addressbooks.into());
        let ucfg_channel = self.channel("UCFG");
        self.stream.execute(&ucfg_channel.open())?;
        let ucfg_bytes = self.stream.execute(&ucfg_channel.com(ucfg))?;

        let str = String::from_utf8_lossy(&ucfg_bytes);
        debug!("Configuration response: {}", str);
        let json_response = Self::json(&ucfg_bytes);
        self.stream.execute(&ucfg_channel.close())?;
        json_response
    }

    pub fn info(&mut self) -> JSONResult<InfoResponse> {
        let info = CommandKind::INFO;
        let info_channel = self.channel("INFO");
        self.stream.execute(&info_channel.open())?;

        let info_bytes = self.stream.execute(&info_channel.com(info))?;
        let json_response = Self::json(&info_bytes);
        self.stream.execute(&info_channel.close())?;
        json_response
    }

    pub fn face_recognition_params(&mut self) -> JSONResult<serde_json::Value> {
        let frcg = CommandKind::FRCG;
        let frcg_channel = self.channel("FRCG");
        self.stream.execute(&frcg_channel.open())?;

        let frcg_bytes = self.stream.execute(&frcg_channel.com(frcg))?;
        let json_response = Self::json(&frcg_bytes);
        self.stream.execute(&frcg_channel.close())?;
        json_response
    }

    pub fn push_info(
        &mut self,
        apt_subaddress: u16,
        open_door: &Opendoor,
    ) -> JSONResult<serde_json::Value> {
        let push = CommandKind::PUSH(PushInfo {
            apt_address: open_door.apt_address.clone(),
            apt_subaddress: 50, // ??
            device_token: "87cd599d00bd7f83d01b85c67b28daa50314b142e7e6649accade61424131dd6".into(),
            profile_id: apt_subaddress.to_string(),
        });
        let push_channel = self.push_channel();
        self.stream.execute(&push_channel.open())?;

        let push_bytes = self.stream.execute(&push_channel.com(push))?;
        let json_response = Self::json(&push_bytes);
        self.stream.execute(&push_channel.close())?;
        json_response
    }

    pub fn open_door(&mut self, vip: &VipConfig, door_name: &str) -> Result<(), ViperError> {
        let sub = format!("{}{}", vip.apt_address, vip.apt_subaddress);
        let door_item = vip
            .user_parameters
            .opendoor_address_book
            .iter()
            .find(|d| d.name.as_str() == door_name)
            .ok_or(ViperError::Generic("Door not found".to_string()))?;

        let ctpp_channel = self.ctpp_channel();
        self.stream.execute(&ctpp_channel.open(&sub))?;
        debug!("CTPP Channel opened");

        self.stream
            .execute(&ctpp_channel.get_unknown_open_door_message(vip))?;
        debug!("Unknown sent");
        self.stream.read()?;
        debug!("Read 1");

        self.stream
            .execute_no_read(&ctpp_channel.get_open_door_message(vip, door_item, false))?;
        debug!("Open sent (false)");
        self.stream
            .execute_no_read(&ctpp_channel.get_open_door_message(vip, door_item, true))?;
        debug!("Open sent (true)");
        self.stream
            .execute_no_read(&ctpp_channel.get_init_open_door_message(vip, door_item))?;
        debug!("Init sent");
        self.stream.read()?;
        debug!("Read 2");
        self.stream.read()?;
        debug!("Read 3");

        self.stream
            .execute_no_read(&ctpp_channel.get_open_door_message(vip, door_item, false))?;
        self.stream
            .execute_no_read(&ctpp_channel.get_open_door_message(vip, door_item, true))?;

        // Close the remaining channels
        self.stream.execute(&ctpp_channel.close())?;
        Ok(())
    }

    pub fn open_actuator(&mut self, vip: &VipConfig, door_name: &str) -> Result<(), ViperError> {
        let sub = format!("{}{}", vip.apt_address, vip.apt_subaddress);
        let door_item = vip
            .user_parameters
            .actuator_address_book
            .iter()
            .find(|d| d.name.as_str() == door_name)
            .ok_or(ViperError::Generic("Actuator not found".to_string()))?;

        let ctpp_channel = self.ctpp_channel();
        self.stream.execute(&ctpp_channel.open(&sub))?;
        debug!("CTPP Channel opened");

        self.stream
            .execute(&ctpp_channel.get_unknown_open_door_message(vip))?;
        debug!("Unknown sent");
        self.stream.read()?;
        debug!("Read 1");

        self.stream
            .execute_no_read(&ctpp_channel.get_init_open_actuator_message(vip, door_item))?;
        self.stream.read()?;
        debug!("Read 2");
        self.stream.read()?;
        debug!("Read 3");

        self.stream
            .execute_no_read(&ctpp_channel.get_open_actuator_message(vip, door_item, false))?;
        self.stream
            .execute_no_read(&ctpp_channel.get_open_actuator_message(vip, door_item, true))?;

        // Close the remaining channels
        self.stream.execute(&ctpp_channel.close())?;
        Ok(())
    }

    pub async fn start_video(
        &mut self,
        ip: &str,
        port: u16,
        vip: &VipConfig,
        output_file: &str,
        door_name: &str,
    ) -> Result<(), ViperError> {
        let sub = format!("{}{}", vip.apt_address, vip.apt_subaddress);
        let door_item = vip
            .user_parameters
            .opendoor_address_book
            .iter()
            .find(|d| d.name.as_str() == door_name)
            .ok_or(ViperError::Generic("Door not found".to_string()))?;

        let ctpp_channel = self.ctpp_channel();
        self.stream.execute(&ctpp_channel.open(&sub))?;
        debug!("CTPP Channel opened");

        self.stream
            .execute(&ctpp_channel.get_unknown_open_door_message(vip))?;
        debug!("Unknown sent");
        self.stream.read()?;
        debug!("Read 1");
        self.stream
            .execute_no_read(&ctpp_channel.get_init_open_door_message(vip, door_item))?;
        self.stream.read()?;
        debug!("Read 2");

        let info = self.push_info(
            vip.apt_subaddress,
            vip.user_parameters.opendoor_address_book.first().unwrap(),
        )?;

        debug!("Push info received {info:?}");

        let mut udpm_channel = self.udpm_channel();
        let resp = self.stream.execute(&udpm_channel.open())?;
        let udpm_id = resp.last_chunk::<2>().unwrap();
        let mut init = false;
        for _ in 0..4 {
            self.udp.write(&udpm_channel.ping(udpm_id)).await?;
            let mut buf = [0u8; 14];
            if self.udp.read(&mut buf).await.is_ok() {
                Helper::print_buffer(&buf);
                init = true;
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
        if !init {
            self.stream.execute(&udpm_channel.close())?;
            panic!("UDPM channel failed to initialize");
        }

        debug!("RTPC Channel opening: ");
        let rtsp_channel = self.rtpc_channel();
        let resp = self.stream.execute(&rtsp_channel.open())?;
        debug!("RTPC Channel opened: ");
        Helper::print_buffer(&resp);
        debug!("RTPC Channel starting stream");
        let resp = self.stream.execute(&rtsp_channel.start_stream(vip))?;
        Helper::print_buffer(&resp);
        self.stream
            .execute_no_read(&rtsp_channel.start_stream(vip))?;

        let args = Args::builder()
            .no_video(false)
            .no_audio(false)
            .max_packets(100000)
            .remote(ip.into())
            .bind("0.0.0.0".into())
            .audio_output(format!("{}.pcm", output_file).into())
            .video_output(format!("{}.h264", output_file).into())
            .port(port)
            .build();
        debug!("Start audio video recording...");
        read_av_stream(&self.udp, args)
            .await
            .map_err(|e| ViperError::Generic(e.to_string()))?;

        udpm_channel.close();
        rtsp_channel.close();
        Ok(())
    }

    fn channel(&mut self, command: &'static str) -> Channel {
        self.tick();

        Channel::new(&self.control, command)
    }

    fn ctpp_channel(&mut self) -> CTPPChannel {
        self.tick();

        CTPPChannel::new(&self.control)
    }

    fn rtpc_channel(&mut self) -> RTPCChannel {
        self.tick();

        RTPCChannel::new(&self.control)
    }

    fn udpm_channel(&mut self) -> UDPMChannel {
        self.tick();

        UDPMChannel::new(&self.control)
    }

    fn push_channel(&mut self) -> Channel {
        self.tick();

        Channel::new(&self.control, "PUSH")
    }

    fn json<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> JSONResult<T> {
        match serde_json::from_slice(bytes) {
            Ok(json) => Ok(json),
            Err(e) => Err(ViperError::JSONError(e)),
        }
    }

    pub fn shutdown(&mut self) {
        self.stream.die();
    }

    // Move the control byte 1 ahead
    fn tick(&mut self) {
        self.control[0] += 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{command::Command, test_helper::SimpleTcpListener};
    use std::thread;

    #[tokio::test]
    async fn test_tick() {
        let _listener = SimpleTcpListener::new("127.0.0.1:3340");
        let mut client = ViperClient::new("127.0.0.1", 3340).await;

        let c = client.control;
        client.tick();

        assert_eq!(c[0] + 1, client.control[0])
    }

    #[tokio::test]
    async fn test_authorize() {
        let listener = SimpleTcpListener::new("127.0.0.1:3341");
        let mut client = ViperClient::new("127.0.0.1", 3341).await;

        thread::spawn(move || {
            let mocked_open = [
                0xcd, 0xab, 0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x1a, 0x12, 0x00, 0x00,
            ];

            let mocked_json = r#"{
                "message":"access",
                "message-type":"response",
                "message-id":5,
                "response-code":200,
                "response-string":"Access Granted"
            }"#;

            listener.mock_server(vec![
                Command::make(&mocked_open, &[0, 0]),
                Command::make(mocked_json.as_bytes(), &[0, 0]),
                Command::make(&[], &[0, 0]), // Closing the channel
            ])
        });

        let resp = client.authorize("TESTTOKEN").unwrap();
        assert_eq!(resp.response.response_string, "Access Granted");
        assert_eq!(resp.response.response_code, 200)
    }
}
