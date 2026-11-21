//! 引擎进程里和平台无关的处理：收到 DLL 的请求，交给引擎，回复结果。
//! Windows 上的命名管道收发在 winsrv 里，这里只管"请求 → 回复"。

use std::path::PathBuf;

use pinyin_ipc::{Reply, Request, Settings, PROTO};

use crate::lm::Lm;
use crate::session::{Engine, Session};

/// 引擎进程：所有连接共用一个 Engine，每个连接一个 Session。
pub struct Service {
    pub engine: Engine,
    data_dir: PathBuf,
    settings_path: PathBuf,
    settings: Settings,
    /// 现在加载的是不是书面模型（None 表示还没加载）
    loaded_formal: Option<bool>,
}

impl Service {
    pub fn new(engine: Engine, data_dir: PathBuf, settings_path: PathBuf) -> Service {
        let mut s = Service { engine, data_dir, settings_path, settings: Settings::default(), loaded_formal: None };
        s.reload_settings();
        s
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// 重新读设置：换语言模型（日常/书面）、开关中文标点。
    pub fn reload_settings(&mut self) {
        self.settings = Settings::load(&self.settings_path);
        self.engine.config.chinese_punct = self.settings.chinese_punct;
        if self.loaded_formal != Some(self.settings.formal) {
            let name = if self.settings.formal { "pinyin-formal.lm" } else { "pinyin.lm" };
            if let Ok(lm) = Lm::open(&self.data_dir.join(name)) {
                self.engine.set_lm(lm);
                self.loaded_formal = Some(self.settings.formal);
            }
        }
    }

    pub fn handle(&mut self, s: &mut Session, req: Request) -> Reply {
        match req {
            Request::Hello { .. } => Reply::Hello { proto: PROTO },
            Request::Key { key, mods } => {
                let r = self.engine.process_key(s, key, mods);
                Reply::KeyResult { consumed: r.consumed, commit: r.commit, view: self.engine.view(s) }
            }
            Request::TestKey { key, mods } => Reply::Bool(self.engine.would_consume(s, key, mods)),
            Request::Reset => {
                s.reset();
                Reply::Ok
            }
            Request::ClearContext => {
                s.clear_context();
                Reply::Ok
            }
            Request::GetState => Reply::State {
                english_mode: self.engine.english_mode(),
                chinese_punct: self.settings.chinese_punct,
                formal: self.settings.formal,
            },
            Request::ReloadSettings => {
                self.reload_settings();
                Reply::Ok
            }
            Request::SetEnglishMode { on } => {
                self.engine.set_english_mode(on);
                s.reset();
                s.clear_context();
                Reply::Ok
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::{Dict, DictBuilder};
    use crate::userdb::UserDb;
    use pinyin_types::{Key, Mods};

    fn engine() -> Engine {
        let mut b = DictBuilder::new(["ni", "hao"].map(String::from));
        let c = b.code("ni hao").unwrap();
        b.add(&c, "你好", -5.0);
        let c = b.code("ni").unwrap();
        b.add(&c, "你", -4.0);
        Engine::new(Dict::from_bytes(b.build()).unwrap(), UserDb::in_memory())
    }

    #[test]
    fn service_handles_typing() {
        let dir = std::env::temp_dir().join(format!("pyime-ipc-{}", std::process::id()));
        let mut svc = Service::new(engine(), dir.clone(), dir.join("settings.ini"));
        let mut s = Session::new();
        let mut commit = String::new();
        for c in "nihao ".chars() {
            let k = if c == ' ' { Key::Space } else { Key::Char(c) };
            assert_eq!(svc.handle(&mut s, Request::TestKey { key: k, mods: Mods::default() }), Reply::Bool(true));
            if let Reply::KeyResult { commit: Some(t), .. } =
                svc.handle(&mut s, Request::Key { key: k, mods: Mods::default() })
            {
                commit.push_str(&t);
            }
        }
        assert_eq!(commit, "你好");
        // 设置：打开中文标点
        Settings { formal: false, chinese_punct: true }.save(&dir.join("settings.ini")).unwrap();
        assert_eq!(svc.handle(&mut s, Request::ReloadSettings), Reply::Ok);
        match svc.handle(&mut s, Request::Key { key: Key::Char(','), mods: Mods::default() }) {
            Reply::KeyResult { commit, .. } => assert_eq!(commit.as_deref(), Some("，")),
            r => panic!("{r:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn over_a_real_socket() {
        use pinyin_ipc::{call, read_frame, write_frame};
        use std::os::unix::net::UnixStream;
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || {
            let mut svc = Service::new(engine(), PathBuf::from("/nonexistent"), PathBuf::from("/nonexistent/s.ini"));
            let mut s = Session::new();
            while let Ok(f) = read_frame(&mut b) {
                let reply = match Request::decode(&f) {
                    Ok(r) => svc.handle(&mut s, r),
                    Err(e) => Reply::Error(e.to_string()),
                };
                if write_frame(&mut b, &reply.encode()).is_err() {
                    break;
                }
            }
        });
        assert_eq!(call(&mut a, &Request::Hello { proto: PROTO, pid: 1 }).unwrap(), Reply::Hello { proto: PROTO });
        for c in "ni".chars() {
            call(&mut a, &Request::Key { key: Key::Char(c), mods: Mods::default() }).unwrap();
        }
        match call(&mut a, &Request::Key { key: Key::Space, mods: Mods::default() }).unwrap() {
            Reply::KeyResult { commit, .. } => assert_eq!(commit.as_deref(), Some("你")),
            r => panic!("{r:?}"),
        }
        drop(a);
        t.join().unwrap();
    }
}
