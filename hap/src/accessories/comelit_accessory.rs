use anyhow::Result;

pub trait ComelitAccessory<T> {
    fn get_comelit_id(&self) -> &str;

    fn update(&mut self, data: &T) -> impl Future<Output = Result<()>>;
}
