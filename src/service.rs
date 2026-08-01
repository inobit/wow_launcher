#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceKind {
    Mysql,
    Auth,
    World,
    Client,
}

impl ServiceKind {
    pub const ALL: [ServiceKind; 4] = [
        ServiceKind::Mysql,
        ServiceKind::Auth,
        ServiceKind::World,
        ServiceKind::Client,
    ];

    pub const SERVICES: [ServiceKind; 3] = [ServiceKind::Mysql, ServiceKind::Auth, ServiceKind::World];

    pub fn label(self) -> &'static str {
        match self {
            ServiceKind::Mysql => "MySQL",
            ServiceKind::Auth => "Auth Server",
            ServiceKind::World => "World Server",
            ServiceKind::Client => "客户端",
        }
    }

    pub fn index(self) -> usize {
        match self {
            ServiceKind::Mysql => 0,
            ServiceKind::Auth => 1,
            ServiceKind::World => 2,
            ServiceKind::Client => 3,
        }
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            ServiceKind::Mysql => "mysqld.exe 完整路径",
            ServiceKind::Auth => "auth_server 可执行文件路径",
            ServiceKind::World => "world_server 可执行文件路径",
            ServiceKind::Client => "wow.exe 完户端路径",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Starting,
    Running,
    Stopping,
    Waiting,
    Error,
}

impl Status {
    pub fn text(self) -> &'static str {
        match self {
            Status::Stopped => "已停止",
            Status::Starting => "启动中…",
            Status::Running => "运行中",
            Status::Stopping => "停止中…",
            Status::Waiting => "等待 MySQL 启动…",
            Status::Error => "错误",
        }
    }

    #[allow(dead_code)]
    pub fn is_operational(self) -> bool {
        matches!(self, Status::Starting | Status::Running | Status::Stopping)
    }
}