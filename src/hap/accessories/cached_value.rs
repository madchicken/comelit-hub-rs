#[allow(dead_code)]
pub struct CachedValue<T: Clone> {
    value: Option<T>,
    created_at: std::time::Instant,
    ttl: std::time::Duration,
}

#[allow(dead_code)]
impl<T: Clone> CachedValue<T> {
    pub fn new(value: T, ttl: std::time::Duration) -> Self {
        Self {
            value: Some(value),
            created_at: std::time::Instant::now(),
            ttl,
        }
    }

    pub fn get(&self) -> Option<T> {
        if self.is_valid() {
            self.value.clone()
        } else {
            None
        }
    }

    pub fn set(&mut self, value: T) {
        self.value = Some(value);
        self.created_at = std::time::Instant::now();
    }

    pub fn is_valid(&self) -> bool {
        self.created_at.elapsed() < self.ttl
    }
}
