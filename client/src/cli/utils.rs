use comelit_hub_rs::{
    ComelitClient, ComelitClientError, ComelitObserver, ComelitOptions, get_secrets,
};

use crate::Params;

pub async fn create_client(
    params: Params,
    observer: Option<ComelitObserver>,
) -> Result<ComelitClient, ComelitClientError> {
    let (mqtt_user, mqtt_password) = get_secrets();
    let options = ComelitOptions::builder()
        .user(params.user)
        .password(params.password)
        .mqtt_user(mqtt_user)
        .mqtt_password(mqtt_password)
        .port(params.port)
        .host(params.host)
        .build()
        .map_err(|e| ComelitClientError::Generic(e.to_string()))?;
    ComelitClient::new(options, observer).await
}
