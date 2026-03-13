use std::net::IpAddr;
use ipnet::IpNet;

/// Check if an IP address falls within any of the given subnets.
pub fn ip_in_any_subnet(ip: IpAddr, subnets: &[IpNet]) -> bool {
    subnets.iter().any(|net| net.contains(&ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_in_cidr() {
        let subnets: Vec<IpNet> = vec!["192.168.1.0/24".parse().unwrap()];
        let ip: IpAddr = "192.168.1.5".parse().unwrap();
        assert!(ip_in_any_subnet(ip, &subnets));
    }

    #[test]
    fn ipv4_not_in_cidr() {
        let subnets: Vec<IpNet> = vec!["192.168.1.0/24".parse().unwrap()];
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(!ip_in_any_subnet(ip, &subnets));
    }

    #[test]
    fn ipv6_in_cidr() {
        let subnets: Vec<IpNet> = vec!["::1/128".parse().unwrap()];
        let ip: IpAddr = "::1".parse().unwrap();
        assert!(ip_in_any_subnet(ip, &subnets));
    }

    #[test]
    fn multiple_subnets() {
        let subnets: Vec<IpNet> = vec![
            "192.168.1.0/24".parse().unwrap(),
            "10.0.0.0/8".parse().unwrap(),
        ];
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(ip_in_any_subnet(ip, &subnets));
    }

    #[test]
    fn empty_subnet_list() {
        let subnets: Vec<IpNet> = vec![];
        let ip: IpAddr = "192.168.1.5".parse().unwrap();
        assert!(!ip_in_any_subnet(ip, &subnets));
    }
}
