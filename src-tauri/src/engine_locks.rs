use crate::config::Engine;
use std::sync::Mutex;

#[derive(Default)]
pub struct EngineLocks([Mutex<()>; 3]);

impl EngineLocks {
    pub fn for_engine(&self, engine: Engine) -> &Mutex<()> {
        &self.0[match engine {
            Engine::Apple => 0,
            Engine::Kokoro => 1,
            Engine::Chatterbox => 2,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_chatterbox_does_not_block_kokoro() {
        let locks = EngineLocks::default();
        let _old = locks.for_engine(Engine::Chatterbox).lock().unwrap();
        assert!(locks.for_engine(Engine::Kokoro).try_lock().is_ok());
        assert!(locks.for_engine(Engine::Chatterbox).try_lock().is_err());
    }
}
