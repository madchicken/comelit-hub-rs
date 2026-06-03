use rs_matter::error::Error;
use rs_matter::transport::network::mdns::astro::AstroMdnsResponder;
use rs_matter::Matter;

pub async fn run_mdns(matter: &Matter<'_>) -> Result<(), Error> {
    AstroMdnsResponder::new().run(matter).await
}
