//! `k8s.io/kubernetes/pkg/apis/networking/validation` for NetworkPolicy.

use std::net::IpAddr;

use serde_json::Value;

use super::field::{forbidden, invalid, not_supported, required, Bad, ErrorList, Path};
use super::meta;
use super::names::{is_valid_port_name, is_valid_port_num, NameRule};
use super::{arr, has, str_of};

pub fn validate_network_policy(np: &Value) -> ErrorList {
    let mut errs = meta::object_meta(&np["metadata"], true, NameRule::Subdomain);
    errs.extend(spec(&np["spec"], &Path::new("spec")));
    errs
}

pub fn validate_network_policy_update(new: &Value, old: &Value) -> ErrorList {
    let mut errs = meta::object_meta_update(&new["metadata"], &old["metadata"]);
    errs.extend(spec(&new["spec"], &Path::new("spec")));
    errs
}

/// `ValidateNetworkPolicySpec`.
fn spec(spec: &Value, path: &Path) -> ErrorList {
    let mut errs = meta::label_selector(&spec["podSelector"], &path.child("podSelector"));
    for (direction, peers) in [("ingress", "from"), ("egress", "to")] {
        for (i, rule) in arr(spec, direction).iter().enumerate() {
            let rule_path = path.child(direction).index(i);
            for (j, port) in arr(rule, "ports").iter().enumerate() {
                errs.extend(policy_port(port, &rule_path.child("ports").index(j)));
            }
            for (j, peer) in arr(rule, peers).iter().enumerate() {
                errs.extend(policy_peer(peer, &rule_path.child(peers).index(j)));
            }
        }
    }
    let types = arr(spec, "policyTypes");
    if types.len() > 2 {
        let bad = Bad::json(&Value::Array(types.to_vec()));
        errs.push(invalid(
            &path.child("policyTypes"),
            bad,
            "may not specify more than two policyTypes",
        ));
        return errs;
    }
    for (i, t) in types.iter().enumerate() {
        let t = t.as_str().unwrap_or_default();
        if t != "Ingress" && t != "Egress" {
            errs.push(not_supported(
                &path.child("policyTypes").index(i),
                Bad::str(t),
                &["Ingress", "Egress"],
            ));
        }
    }
    errs
}

/// `ValidateNetworkPolicyPort`.
fn policy_port(port: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    if let Some(protocol) = port.get("protocol").and_then(Value::as_str) {
        if !["TCP", "UDP", "SCTP"].contains(&protocol) {
            errs.push(not_supported(
                &path.child("protocol"),
                Bad::str(protocol),
                &["TCP", "UDP", "SCTP"],
            ));
        }
    }
    let end_port = port.get("endPort").and_then(Value::as_i64);
    match port.get("port") {
        Some(Value::String(name)) => {
            if let Some(end) = end_port {
                errs.push(invalid(
                    &path.child("endPort"),
                    Bad::int(end),
                    "may not be specified when `port` is non-numeric",
                ));
            }
            for m in is_valid_port_name(name) {
                errs.push(invalid(&path.child("port"), Bad::str(name), &m));
            }
        }
        Some(Value::Number(n)) => {
            let n = n.as_i64().unwrap_or_default();
            for m in is_valid_port_num(n) {
                errs.push(invalid(&path.child("port"), Bad::int(n), &m));
            }
            if let Some(end) = end_port {
                if end < n {
                    errs.push(invalid(
                        &path.child("endPort"),
                        Bad::int(n),
                        "must be greater than or equal to `port`",
                    ));
                }
                for m in is_valid_port_num(end) {
                    errs.push(invalid(&path.child("endPort"), Bad::int(end), &m));
                }
            }
        }
        _ => {
            if let Some(end) = end_port {
                errs.push(invalid(
                    &path.child("endPort"),
                    Bad::int(end),
                    "may not be specified when `port` is not specified",
                ));
            }
        }
    }
    errs
}

/// `ValidateNetworkPolicyPeer`.
fn policy_peer(peer: &Value, path: &Path) -> ErrorList {
    let mut errs = Vec::new();
    let mut count = 0;
    for key in ["podSelector", "namespaceSelector"] {
        if has(peer, key) {
            count += 1;
            errs.extend(meta::label_selector(&peer[key], &path.child(key)));
        }
    }
    if has(peer, "ipBlock") {
        count += 1;
        errs.extend(ip_block(&peer["ipBlock"], &path.child("ipBlock")));
    }
    if count == 0 {
        errs.push(required(path, "must specify a peer"));
    } else if count > 1 && has(peer, "ipBlock") {
        errs.push(forbidden(path, "may not specify both ipBlock and another peer"));
    }
    errs
}

/// A CIDR as `netutils.ParseCIDRSloppy` reads it: (address, prefix length, bits).
fn parse_cidr(cidr: &str) -> Option<(IpAddr, u32, u32)> {
    let (ip, len) = cidr.split_once('/')?;
    let ip: IpAddr = ip.parse().ok()?;
    let bits = if ip.is_ipv4() { 32 } else { 128 };
    let len: u32 = len.parse().ok().filter(|l| *l <= bits)?;
    Some((ip, len, bits))
}

const CIDR_ERR: &str = "must be a valid CIDR value, (e.g. 10.9.8.0/24 or 2001:db8::/64)";

/// `ValidateIPBlock`.
fn ip_block(block: &Value, path: &Path) -> ErrorList {
    let cidr = str_of(block, "cidr");
    if cidr.is_empty() {
        return vec![required(&path.child("cidr"), "")];
    }
    let mut errs = Vec::new();
    let Some((net, len, _)) = parse_cidr(cidr) else {
        errs.push(invalid(&path.child("cidr"), Bad::str(cidr), CIDR_ERR));
        return errs;
    };
    for (i, except) in arr(block, "except").iter().enumerate() {
        let except = except.as_str().unwrap_or_default();
        let p = path.child("except").index(i);
        let Some((ip, except_len, _)) = parse_cidr(except) else {
            errs.push(invalid(&p, Bad::str(except), CIDR_ERR));
            continue;
        };
        if !contains(net, len, ip) || len >= except_len {
            errs.push(invalid(&p, Bad::str(except), "must be a strict subset of `cidr`"));
        }
    }
    errs
}

fn contains(net: IpAddr, len: u32, ip: IpAddr) -> bool {
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) => {
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            u32::from(n) & mask == u32::from(i) & mask
        }
        (IpAddr::V6(n), IpAddr::V6(i)) => {
            let mask = if len == 0 { 0 } else { u128::MAX << (128 - len) };
            u128::from(n) & mask == u128::from(i) & mask
        }
        _ => false,
    }
}
