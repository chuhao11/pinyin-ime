//! 端到端：启动真正的引擎程序，像输入法 DLL 一样连上去打字。
//! Windows 上走真正的命名管道（含权限和所有者检查），Mac/Linux 上走本地套接字。

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use pinyin_ipc::pinyin_types::{Key, Mods};
use pinyin_ipc::{call, Reply, Request, Settings, PROTO};

struct Engine(Child);

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn data_dir() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/build");
    d.join("pinyin.dict").exists().then_some(d)
}

#[cfg(unix)]
type Conn = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Conn = pinyin_winpipe::PipeConn;

fn connect(_sock: &std::path::Path) -> Option<Conn> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        #[cfg(unix)]
        let c = std::os::unix::net::UnixStream::connect(_sock).ok();
        #[cfg(windows)]
        let c = pinyin_winpipe::connect(5000).ok();
        if c.is_some() {
            return c;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}

fn typ(c: &mut Conn, s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        let key = if ch == ' ' { Key::Space } else { Key::Char(ch) };
        match call(c, &Request::Key { key, mods: Mods::default() }).unwrap() {
            Reply::KeyResult { consumed, commit, .. } => {
                if let Some(t) = commit {
                    out.push_str(&t);
                } else if !consumed {
                    out.push(ch);
                }
            }
            r => panic!("意外的回复：{r:?}"),
        }
    }
    out
}

#[test]
fn type_through_the_real_engine_process() {
    let Some(data) = data_dir() else {
        if std::env::var("REQUIRE_DICT").is_ok() {
            panic!("找不到词库");
        }
        eprintln!("跳过：没有词库");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("pyime-e2e-{}", std::process::id()));
    let user = tmp.join("user");
    std::fs::create_dir_all(&user).unwrap();
    let sock = tmp.join("sock");
    let _engine = Engine(
        Command::new(env!("CARGO_BIN_EXE_pinyin-engine"))
            .env("PINYIN_DATA", &data)
            .env("PINYIN_USER_DIR", &user)
            .env("PINYIN_SOCKET", &sock)
            .spawn()
            .unwrap(),
    );
    let mut c = connect(&sock).expect("连不上引擎");
    assert_eq!(call(&mut c, &Request::Hello { proto: PROTO, pid: 1 }).unwrap(), Reply::Hello { proto: PROTO });
    assert_eq!(typ(&mut c, "nihao "), "你好");
    assert_eq!(typ(&mut c, "nihao,"), "你好,");

    // 第二个程序同时连上来，各打各的，互不干扰
    let mut c2 = connect(&sock).expect("第二个连接连不上");
    typ(&mut c2, "wo");
    assert_eq!(typ(&mut c, "shi "), "是");
    assert_eq!(typ(&mut c2, " "), "我");

    // 设置页改了设置文件：两秒内生效
    Settings { chinese_punct: true }.save(&user.join("settings.ini")).unwrap();
    let start = Instant::now();
    loop {
        if typ(&mut c, "nihao,") == "你好，" {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "设置没有生效");
        std::thread::sleep(Duration::from_millis(300));
    }
    // 乱码请求：回复错误，不崩溃，连接还能继续用
    pinyin_ipc::write_frame(&mut c, &[200, 1, 2, 3]).unwrap();
    let r = pinyin_ipc::read_frame(&mut c).unwrap();
    assert!(matches!(Reply::decode(&r).unwrap(), Reply::Error(_)));
    assert_eq!(typ(&mut c, "shi "), "是");

    // 设置页"添加词语"：加进去，用全拼打排第二个
    match call(&mut c, &Request::AddWords { text: "卷王，abc".into() }).unwrap() {
        Reply::WordsAdded { added, skipped } => {
            assert_eq!(added, 1);
            assert_eq!(skipped.len(), 1, "{skipped:?}");
        }
        r => panic!("{r:?}"),
    }
    let view = |c: &mut Conn, s: &str| -> Vec<String> {
        let mut items = Vec::new();
        for ch in s.chars() {
            if let Ok(Reply::KeyResult { view, .. }) =
                call(c, &Request::Key { key: Key::Char(ch), mods: Mods::default() })
            {
                items = view.items;
            }
        }
        let _ = call(c, &Request::Key { key: Key::Escape, mods: Mods::default() });
        items
    };
    let items = view(&mut c, "juanwang");
    assert!(items.iter().take(2).any(|t| t == "卷王"), "{items:?}");

    // 记忆存在用户目录里
    assert!(user.join("user.db").exists());
    drop(c);
    drop(c2);
    drop(_engine);

    // 引擎没运行时设置页把词存进等待文件：引擎启动时加进去
    std::fs::write(user.join(pinyin_ipc::PENDING_WORDS), "螺蛳粉卷\n").unwrap();
    let sock2 = tmp.join("sock2");
    let _engine2 = Engine(
        Command::new(env!("CARGO_BIN_EXE_pinyin-engine"))
            .env("PINYIN_DATA", &data)
            .env("PINYIN_USER_DIR", &user)
            .env("PINYIN_SOCKET", &sock2)
            .spawn()
            .unwrap(),
    );
    let mut c = connect(&sock2).expect("连不上引擎");
    call(&mut c, &Request::Hello { proto: PROTO, pid: 1 }).unwrap();
    let items = view(&mut c, "luosifenjuan");
    assert!(items.iter().take(2).any(|t| t == "螺蛳粉卷"), "{items:?}");
    assert!(!user.join(pinyin_ipc::PENDING_WORDS).exists(), "等待文件读完就删");
    let _ = std::fs::remove_dir_all(&tmp);
}
