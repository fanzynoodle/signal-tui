use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Contact {
    pub number: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub conversation_key: String,
    pub source: Option<String>,
    pub timestamp_ms: Option<i64>,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct SignalCli {
    bin: String,
}

impl Default for SignalCli {
    fn default() -> Self {
        Self {
            bin: "signal-cli".to_string(),
        }
    }
}

impl SignalCli {
    pub fn with_bin(bin: impl Into<String>) -> Self {
        Self { bin: bin.into() }
    }

    pub fn list_accounts(&self) -> Result<Vec<String>> {
        #[derive(Debug, Deserialize)]
        struct Account {
            number: String,
        }
        let v = self.run_json(["-o", "json", "listAccounts"])?;
        let Some(v) = v else { return Ok(vec![]); };
        let accounts: Vec<Account> =
            serde_json::from_value(v).context("parse listAccounts JSON")?;
        Ok(accounts.into_iter().map(|a| a.number).collect())
    }

    pub fn list_contacts(&self, account: &str) -> Result<Vec<Contact>> {
        #[derive(Debug, Deserialize)]
        struct ContactJson {
            number: Option<String>,
            name: Option<String>,
        }

        let v = self.run_json(["-a", account, "-o", "json", "listContacts"])?;
        let Some(v) = v else { return Ok(vec![]); };
        let raw: Vec<ContactJson> =
            serde_json::from_value(v).context("parse listContacts JSON")?;
        let mut out = Vec::new();
        for c in raw {
            let Some(number) = c.number else { continue; };
            let name = c.name.and_then(|s| {
                let t = s.trim();
                if t.is_empty() { None } else { Some(t.to_string()) }
            });
            out.push(Contact {
                number,
                name,
            });
        }
        Ok(out)
    }

    pub fn list_groups(&self, account: &str) -> Result<Vec<Group>> {
        let v = self.run_json(["-a", account, "-o", "json", "listGroups"])?;
        let Some(v) = v else { return Ok(vec![]); };
        let arr = v.as_array().context("listGroups JSON was not an array")?;
        let mut out = Vec::new();
        for g in arr {
            let obj = match g.as_object() {
                Some(o) => o,
                None => continue,
            };
            let id = obj
                .get("id")
                .and_then(|v| v.as_str())
                .or_else(|| obj.get("groupId").and_then(|v| v.as_str()))
                .or_else(|| obj.get("group_id").and_then(|v| v.as_str()));
            let Some(id) = id else { continue; };
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .and_then(|s| {
                    let t = s.trim();
                    if t.is_empty() { None } else { Some(t.to_string()) }
                });
            out.push(Group { id: id.to_string(), name });
        }
        Ok(out)
    }

    pub fn send_message_to_number(&self, account: &str, recipient: &str, body: &str) -> Result<()> {
        self.run_status(["-a", account, "send", "-m", body, recipient])
            .with_context(|| format!("send message to {recipient}"))?;
        Ok(())
    }

    pub fn send_message_to_group(&self, account: &str, group_id: &str, body: &str) -> Result<()> {
        self.run_status(["-a", account, "send", "-g", group_id, "-m", body])
            .with_context(|| format!("send message to group {group_id}"))?;
        Ok(())
    }

    pub fn receive_once(&self, account: &str, timeout_secs: u64) -> Result<Vec<IncomingMessage>> {
        let timeout = timeout_secs.to_string();
        let v = self.run_json(["-a", account, "-o", "json", "receive", "--timeout", &timeout])?;
        let Some(v) = v else { return Ok(vec![]); };
        self.parse_receive_json(v)
    }

    fn parse_receive_json(&self, v: Value) -> Result<Vec<IncomingMessage>> {
        // `signal-cli -o json receive` format is not fully stable across versions; parse defensively.
        let items: Vec<Value> = match v {
            Value::Array(a) => a,
            other => vec![other],
        };

        let mut out = Vec::new();
        for item in items {
            let Some(obj) = item.as_object() else { continue; };
            let env = obj.get("envelope").unwrap_or(&Value::Null);
            let env_obj = env.as_object();

            let timestamp_ms = env_obj
                .and_then(|e| e.get("timestamp").and_then(|t| t.as_i64()))
                .or_else(|| obj.get("timestamp").and_then(|t| t.as_i64()));

            let source_number = env_obj
                .and_then(|e| {
                    e.get("sourceNumber")
                        .and_then(|s| s.as_str())
                        .or_else(|| e.get("source").and_then(|s| s.as_str()))
                })
                .map(|s| s.to_string());

            let data_msg = env_obj
                .and_then(|e| e.get("dataMessage"))
                .or_else(|| obj.get("dataMessage"))
                .unwrap_or(&Value::Null);

            let body = data_msg
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            if body.is_empty() {
                // Ignore non-text events for now (typing, receipts, etc.)
                continue;
            }

            let group_id = data_msg
                .get("groupInfo")
                .and_then(|g| g.get("groupId").and_then(|s| s.as_str()))
                .or_else(|| {
                    data_msg
                        .get("groupInfo")
                        .and_then(|g| g.get("group_id").and_then(|s| s.as_str()))
                })
                .map(|s| s.to_string());

            let conversation_key = if let Some(gid) = group_id {
                format!("group:{gid}")
            } else if let Some(src) = &source_number {
                format!("contact:{src}")
            } else {
                // Unknown; keep it bucketed.
                "unknown:unknown".to_string()
            };

            out.push(IncomingMessage {
                conversation_key,
                source: source_number,
                timestamp_ms,
                body,
            });
        }
        Ok(out)
    }

    fn run_status<const N: usize>(&self, args: [&str; N]) -> Result<()> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args);
        let output = cmd
            .output()
            .with_context(|| format!("failed to execute {}", self.bin))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "signal-cli failed (code={:?}). stderr: {} stdout: {}",
            output.status.code(),
            stderr.trim(),
            stdout.trim()
        );
    }

    fn run_json<const N: usize>(&self, args: [&str; N]) -> Result<Option<Value>> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args);
        let output = cmd
            .output()
            .with_context(|| format!("failed to execute {}", self.bin))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!(
                "signal-cli failed (code={:?}). stderr: {} stdout: {}",
                output.status.code(),
                stderr.trim(),
                stdout.trim()
            );
        }

        let stdout = String::from_utf8(output.stdout).context("signal-cli output was not utf-8")?;
        let s = stdout.trim();
        if s.is_empty() {
            return Ok(None);
        }

        // Prefer parsing as a single JSON value; fall back to JSON-per-line.
        if let Ok(v) = serde_json::from_str::<Value>(s) {
            return Ok(Some(v));
        }

        let mut items = Vec::new();
        for line in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let v: Value = serde_json::from_str(line)
                .with_context(|| format!("failed to parse JSON line from signal-cli: {line}"))?;
            items.push(v);
        }
        Ok(Some(Value::Array(items)))
    }
}
