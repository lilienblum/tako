use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU16, Ordering};

#[cfg(target_os = "linux")]
use parking_lot::Mutex;

const LOOPBACK_BIND_HOST: &str = "127.0.0.1";
#[cfg(target_os = "linux")]
const NAMESPACE_BIND_HOST: &str = "0.0.0.0";
const UNSAFE_HOST_UPSTREAM_ENV: &str = "TAKO_UNSAFE_HOST_UPSTREAM";
const NAMESPACE_APP_PORT: u16 = 3000;

#[cfg(target_os = "linux")]
const NAMESPACE_NETNS_DIR: &str = "/run/netns";
#[cfg(target_os = "linux")]
const NAMESPACE_SUBNET_CIDR: &str = "10.233.0.0/16";
#[cfg(target_os = "linux")]
const MAX_NAMESPACE_SUBNETS: u16 = 16_384;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamEndpoint {
    addr: SocketAddr,
    bind_host: String,
}

impl UpstreamEndpoint {
    pub fn loopback(port: u16) -> Self {
        Self {
            addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)),
            bind_host: LOOPBACK_BIND_HOST.to_string(),
        }
    }

    #[cfg(target_os = "linux")]
    fn namespaced(ip: Ipv4Addr) -> Self {
        Self {
            addr: SocketAddr::V4(SocketAddrV4::new(ip, NAMESPACE_APP_PORT)),
            bind_host: NAMESPACE_BIND_HOST.to_string(),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn bind_host(&self) -> &str {
        &self.bind_host
    }
}

pub struct PreparedInstanceNetwork {
    endpoint: UpstreamEndpoint,
    mode: PreparedInstanceNetworkMode,
}

impl PreparedInstanceNetwork {
    pub fn host_loopback(port: u16) -> Self {
        Self {
            endpoint: UpstreamEndpoint::loopback(port),
            mode: PreparedInstanceNetworkMode::HostLoopback,
        }
    }

    #[cfg(target_os = "linux")]
    fn linux_namespace(plan: LinuxNamespacePlan, allocator: Arc<SubnetAllocator>) -> Self {
        let endpoint = plan.endpoint();
        Self {
            endpoint,
            mode: PreparedInstanceNetworkMode::LinuxNamespace(LinuxNamespaceRuntime {
                plan,
                allocator,
            }),
        }
    }

    pub fn endpoint(&self) -> &UpstreamEndpoint {
        &self.endpoint
    }

    pub fn bind_host(&self) -> &str {
        self.endpoint.bind_host()
    }

    pub fn cleanup(self) {
        match self.mode {
            PreparedInstanceNetworkMode::HostLoopback => {}
            #[cfg(target_os = "linux")]
            PreparedInstanceNetworkMode::LinuxNamespace(runtime) => runtime.cleanup(),
        }
    }

    pub fn namespace_path(&self) -> Option<PathBuf> {
        match &self.mode {
            PreparedInstanceNetworkMode::HostLoopback => None,
            #[cfg(target_os = "linux")]
            PreparedInstanceNetworkMode::LinuxNamespace(runtime) => {
                Some(runtime.plan.namespace_path())
            }
        }
    }
}

enum PreparedInstanceNetworkMode {
    HostLoopback,
    #[cfg(target_os = "linux")]
    LinuxNamespace(LinuxNamespaceRuntime),
}

#[cfg(target_os = "linux")]
struct LinuxNamespaceRuntime {
    plan: LinuxNamespacePlan,
    allocator: Arc<SubnetAllocator>,
}

#[cfg(target_os = "linux")]
impl LinuxNamespaceRuntime {
    fn cleanup(self) {
        self.plan.cleanup(&SystemLinuxCommandRunner);
        self.allocator.release(self.plan.subnet_index());
    }
}

pub struct UpstreamManager {
    #[cfg(target_os = "linux")]
    allocator: Arc<SubnetAllocator>,
}

impl UpstreamManager {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            allocator: Arc::new(SubnetAllocator::default()),
        }
    }

    pub fn prepare(&self, _instance_id: &str) -> io::Result<PreparedInstanceNetwork> {
        #[cfg(target_os = "linux")]
        if running_as_root() {
            let subnet_index = self.allocator.allocate()?;
            let plan = LinuxNamespacePlan::new(_instance_id, subnet_index)?;
            if let Err(error) = plan.setup(&SystemLinuxCommandRunner) {
                self.allocator.release(subnet_index);
                return Err(error);
            }
            return Ok(PreparedInstanceNetwork::linux_namespace(
                plan,
                self.allocator.clone(),
            ));
        }

        if allow_unsafe_host_upstream() {
            return reserve_host_loopback_network();
        }

        Err(io::Error::other(
            "TCP app upstreams require Linux root for namespace isolation. Set TAKO_UNSAFE_HOST_UPSTREAM=1 only for local debug/test use.",
        ))
    }
}

impl Default for UpstreamManager {
    fn default() -> Self {
        Self::new()
    }
}

fn allow_unsafe_host_upstream() -> bool {
    std::env::var_os(UNSAFE_HOST_UPSTREAM_ENV)
        .map(|value| !value.is_empty())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn running_as_root() -> bool {
    // SAFETY: geteuid has no safety preconditions.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(target_os = "linux"))]
fn running_as_root() -> bool {
    false
}

fn reserve_host_loopback_network() -> io::Result<PreparedInstanceNetwork> {
    let listener = std::net::TcpListener::bind((LOOPBACK_BIND_HOST, 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(PreparedInstanceNetwork::host_loopback(port))
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinuxNamespacePlan {
    namespace: String,
    host_veth: String,
    child_veth: String,
    host_ip: Ipv4Addr,
    child_ip: Ipv4Addr,
    subnet_index: u16,
}

#[cfg(target_os = "linux")]
impl LinuxNamespacePlan {
    fn new(instance_id: &str, subnet_index: u16) -> io::Result<Self> {
        let namespace = format!("tako-{instance_id}");
        let host_veth = format!("tkh{instance_id}");
        let child_veth = format!("tkc{instance_id}");
        if host_veth.len() > 15 || child_veth.len() > 15 {
            return Err(io::Error::other(format!(
                "instance id '{}' is too long for Linux veth naming",
                instance_id
            )));
        }
        let (host_ip, child_ip) = subnet_ips(subnet_index)?;
        Ok(Self {
            namespace,
            host_veth,
            child_veth,
            host_ip,
            child_ip,
            subnet_index,
        })
    }

    fn endpoint(&self) -> UpstreamEndpoint {
        UpstreamEndpoint::namespaced(self.child_ip)
    }

    fn namespace_path(&self) -> PathBuf {
        Path::new(NAMESPACE_NETNS_DIR).join(&self.namespace)
    }

    fn subnet_index(&self) -> u16 {
        self.subnet_index
    }

    fn setup<R: LinuxCommandRunner>(&self, runner: &R) -> io::Result<()> {
        ensure_linux_host_networking(runner)?;

        let setup = (|| {
            runner.run("ip", &["netns", "add", self.namespace.as_str()])?;
            runner.run(
                "ip",
                &[
                    "link",
                    "add",
                    self.host_veth.as_str(),
                    "type",
                    "veth",
                    "peer",
                    "name",
                    self.child_veth.as_str(),
                ],
            )?;
            runner.run(
                "ip",
                &[
                    "link",
                    "set",
                    self.child_veth.as_str(),
                    "netns",
                    self.namespace.as_str(),
                ],
            )?;
            runner.run(
                "ip",
                &[
                    "addr",
                    "add",
                    &format!("{}/30", self.host_ip),
                    "dev",
                    self.host_veth.as_str(),
                ],
            )?;
            runner.run("ip", &["link", "set", self.host_veth.as_str(), "up"])?;
            runner.run(
                "ip",
                &[
                    "-n",
                    self.namespace.as_str(),
                    "addr",
                    "add",
                    &format!("{}/30", self.child_ip),
                    "dev",
                    self.child_veth.as_str(),
                ],
            )?;
            runner.run(
                "ip",
                &[
                    "-n",
                    self.namespace.as_str(),
                    "link",
                    "set",
                    self.child_veth.as_str(),
                    "up",
                ],
            )?;
            runner.run(
                "ip",
                &["-n", self.namespace.as_str(), "link", "set", "lo", "up"],
            )?;
            runner.run(
                "ip",
                &[
                    "-n",
                    self.namespace.as_str(),
                    "route",
                    "add",
                    "default",
                    "via",
                    &self.host_ip.to_string(),
                    "dev",
                    self.child_veth.as_str(),
                ],
            )?;
            Ok(())
        })();

        if setup.is_err() {
            self.cleanup(runner);
        }

        setup
    }

    fn cleanup<R: LinuxCommandRunner>(&self, runner: &R) {
        let _ = runner.run("ip", &["link", "del", self.host_veth.as_str()]);
        let _ = runner.run("ip", &["netns", "delete", self.namespace.as_str()]);
    }
}

#[cfg(target_os = "linux")]
fn ensure_linux_host_networking<R: LinuxCommandRunner>(runner: &R) -> io::Result<()> {
    runner.run("sysctl", &["-w", "net.ipv4.ip_forward=1"])?;

    ensure_iptables_rule(
        runner,
        &["-C", "FORWARD", "-s", NAMESPACE_SUBNET_CIDR, "-j", "ACCEPT"],
        &["-A", "FORWARD", "-s", NAMESPACE_SUBNET_CIDR, "-j", "ACCEPT"],
    )?;
    ensure_iptables_rule(
        runner,
        &[
            "-C",
            "FORWARD",
            "-d",
            NAMESPACE_SUBNET_CIDR,
            "-m",
            "conntrack",
            "--ctstate",
            "ESTABLISHED,RELATED",
            "-j",
            "ACCEPT",
        ],
        &[
            "-A",
            "FORWARD",
            "-d",
            NAMESPACE_SUBNET_CIDR,
            "-m",
            "conntrack",
            "--ctstate",
            "ESTABLISHED,RELATED",
            "-j",
            "ACCEPT",
        ],
    )?;
    ensure_iptables_rule_with_table(
        runner,
        "nat",
        &[
            "-C",
            "POSTROUTING",
            "-s",
            NAMESPACE_SUBNET_CIDR,
            "!",
            "-d",
            NAMESPACE_SUBNET_CIDR,
            "-j",
            "MASQUERADE",
        ],
        &[
            "-A",
            "POSTROUTING",
            "-s",
            NAMESPACE_SUBNET_CIDR,
            "!",
            "-d",
            NAMESPACE_SUBNET_CIDR,
            "-j",
            "MASQUERADE",
        ],
    )?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_iptables_rule<R: LinuxCommandRunner>(
    runner: &R,
    check_args: &[&str],
    add_args: &[&str],
) -> io::Result<()> {
    if runner.check("iptables", check_args)? {
        return Ok(());
    }
    runner.run("iptables", add_args)
}

#[cfg(target_os = "linux")]
fn ensure_iptables_rule_with_table<R: LinuxCommandRunner>(
    runner: &R,
    table: &str,
    check_args: &[&str],
    add_args: &[&str],
) -> io::Result<()> {
    let mut prefixed_check = vec!["-t", table];
    prefixed_check.extend_from_slice(check_args);
    if runner.check("iptables", &prefixed_check)? {
        return Ok(());
    }
    let mut prefixed_add = vec!["-t", table];
    prefixed_add.extend_from_slice(add_args);
    runner.run("iptables", &prefixed_add)
}

#[cfg(target_os = "linux")]
fn subnet_ips(subnet_index: u16) -> io::Result<(Ipv4Addr, Ipv4Addr)> {
    if subnet_index >= MAX_NAMESPACE_SUBNETS {
        return Err(io::Error::other("exhausted available namespace subnets"));
    }
    let offset = u32::from(subnet_index) * 4;
    let third = (offset / 256) as u8;
    let fourth = (offset % 256) as u8;
    let host_ip = Ipv4Addr::new(10, 233, third, fourth.saturating_add(1));
    let child_ip = Ipv4Addr::new(10, 233, third, fourth.saturating_add(2));
    Ok((host_ip, child_ip))
}

#[cfg(target_os = "linux")]
trait LinuxCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<()>;
    fn check(&self, program: &str, args: &[&str]) -> io::Result<bool>;
}

#[cfg(target_os = "linux")]
struct SystemLinuxCommandRunner;

#[cfg(target_os = "linux")]
impl LinuxCommandRunner for SystemLinuxCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<()> {
        let output = std::process::Command::new(program).args(args).output()?;
        if output.status.success() {
            return Ok(());
        }
        Err(io::Error::other(format!(
            "{} {} failed: {}",
            program,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }

    fn check(&self, program: &str, args: &[&str]) -> io::Result<bool> {
        Ok(std::process::Command::new(program)
            .args(args)
            .status()?
            .success())
    }
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct SubnetAllocator {
    next: AtomicU16,
    released: Mutex<Vec<u16>>,
}

#[cfg(target_os = "linux")]
impl SubnetAllocator {
    fn allocate(&self) -> io::Result<u16> {
        if let Some(index) = self.released.lock().pop() {
            return Ok(index);
        }
        let index = self.next.fetch_add(1, Ordering::Relaxed);
        if index >= MAX_NAMESPACE_SUBNETS {
            return Err(io::Error::other("exhausted available namespace subnets"));
        }
        Ok(index)
    }

    fn release(&self, index: u16) {
        self.released.lock().push(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_loopback_endpoint_uses_localhost_bind_host() {
        let upstream = PreparedInstanceNetwork::host_loopback(47_831);
        assert_eq!(
            upstream.endpoint().addr(),
            "127.0.0.1:47831".parse().unwrap()
        );
        assert_eq!(upstream.bind_host(), "127.0.0.1");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn subnet_ips_assign_host_and_child_addresses_from_index() {
        let (host_ip, child_ip) = subnet_ips(2).expect("subnet should allocate");
        assert_eq!(host_ip, Ipv4Addr::new(10, 233, 0, 9));
        assert_eq!(child_ip, Ipv4Addr::new(10, 233, 0, 10));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_namespace_plan_uses_virtual_ip_and_wildcard_bind_host() {
        let plan = LinuxNamespacePlan::new("abcd1234", 1).expect("plan should build");
        assert_eq!(plan.namespace, "tako-abcd1234");
        assert_eq!(plan.host_veth, "tkhabcd1234");
        assert_eq!(plan.child_veth, "tkcabcd1234");
        assert_eq!(plan.endpoint().addr(), "10.233.0.6:3000".parse().unwrap());
        assert_eq!(plan.endpoint().bind_host(), "0.0.0.0");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_namespace_setup_configures_namespace_and_nat() {
        #[derive(Default)]
        struct FakeRunner {
            commands: Mutex<Vec<String>>,
        }

        impl LinuxCommandRunner for FakeRunner {
            fn run(&self, program: &str, args: &[&str]) -> io::Result<()> {
                self.commands
                    .lock()
                    .push(format!("RUN {} {}", program, args.join(" ")));
                Ok(())
            }

            fn check(&self, program: &str, args: &[&str]) -> io::Result<bool> {
                self.commands
                    .lock()
                    .push(format!("CHECK {} {}", program, args.join(" ")));
                Ok(false)
            }
        }

        let runner = FakeRunner::default();
        let plan = LinuxNamespacePlan::new("abcd1234", 0).expect("plan should build");
        plan.setup(&runner).expect("setup should succeed");

        let commands = runner.commands.lock().clone();
        assert!(
            commands
                .iter()
                .any(|command| { command == "RUN sysctl -w net.ipv4.ip_forward=1" })
        );
        assert!(
            commands
                .iter()
                .any(|command| { command == "RUN ip netns add tako-abcd1234" })
        );
        assert!(commands.iter().any(|command| {
            command == "RUN ip -n tako-abcd1234 route add default via 10.233.0.1 dev tkcabcd1234"
        }));
        assert!(commands.iter().any(|command| {
            command == "RUN iptables -t nat -A POSTROUTING -s 10.233.0.0/16 ! -d 10.233.0.0/16 -j MASQUERADE"
        }));
    }
}
