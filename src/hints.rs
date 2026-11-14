//! The next step for every error and warning code, in the configured
//! language. A hint is short and names one concrete action, with the command
//! when there is one, for an agent with a shell and for a person being told
//! what to type. `docs/FOR-AGENTS.md` lists the same catalogue.

use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Pt,
}

static PORTUGUESE: AtomicBool = AtomicBool::new(false);

/// `language` of the config: `pt` is Portuguese, anything else English.
pub fn set_language(language: &str) {
    PORTUGUESE.store(language == "pt", Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if PORTUGUESE.load(Ordering::Relaxed) {
        Lang::Pt
    } else {
        Lang::En
    }
}

/// Reads `language` from the machine's config.json, when there is one.
pub fn load_language() {
    if let Some(language) = crate::config::json_path_for_cli()
        .ok()
        .and_then(|path| crate::config::read_json(&path).ok())
        .and_then(|map| {
            map.get("language")
                .and_then(Value::as_str)
                .map(String::from)
        })
    {
        set_language(&language);
    }
}

const GENERIC: (&str, &str) = (
    "Run `shutup-and-run doctor --report` and open the issue link it prints.",
    "Rode `shutup-and-run doctor --report` e abra o link de issue que ele imprime.",
);

/// Code, English, Portuguese.
pub const CATALOG: &[(&str, &str, &str)] = &[
    // The command line itself.
    ("USAGE", "Wrong command line. Run `shutup-and-run --help` and copy the command from there.", "Linha de comando errada. Rode `shutup-and-run --help` e copie o comando de lá."),
    ("INVALID_VALUE", "Nothing was written. Run `shutup-and-run config keys`, then set the key again with a valid value.", "Nada foi gravado. Rode `shutup-and-run config keys` e defina a chave de novo com um valor válido."),
    ("FAILED", "Run `shutup-and-run doctor` and follow the hint of each failed line.", "Rode `shutup-and-run doctor` e siga a dica de cada linha que falhou."),
    ("NOT_INSTALLED", "Run `shutup-and-run wsl install` in an administrator terminal.", "Rode `shutup-and-run wsl install` em um terminal de administrador."),
    // Config.
    ("CONFIG_INVALID", "Run `shutup-and-run config validate` to see the bad value; fix it with `shutup-and-run config set KEY VALUE`, or `shutup-and-run config reset`.", "Rode `shutup-and-run config validate` para ver o valor errado; corrija com `shutup-and-run config set CHAVE VALOR`, ou `shutup-and-run config reset`."),
    ("CONFIG_FAILED", "Run `shutup-and-run config validate`; if the file cannot be repaired, `shutup-and-run config reset`.", "Rode `shutup-and-run config validate`; se o arquivo não tiver conserto, `shutup-and-run config reset`."),
    ("CONFIG_FALLBACK_IN_USE", "The config file was damaged and a fallback runs. Run `shutup-and-run config reset`, then set the poll URL again.", "O arquivo de config estava danificado e uma cópia reserva está em uso. Rode `shutup-and-run config reset` e defina a poll URL de novo."),
    ("POLL_INTERVAL_RAISED", "Run `shutup-and-run config set poll_interval_secs 30 --apply`.", "Rode `shutup-and-run config set poll_interval_secs 30 --apply`."),
    ("POLL_INTERVAL_TOO_LOW", "Use 30 or more: `--poll-interval-secs 30`, or leave the option out.", "Use 30 ou mais: `--poll-interval-secs 30`, ou não passe a opção."),
    ("STORAGE_NOT_WRITABLE", "Run the command as root (Linux: `sudo`), or point `history_path` at a writable folder with `shutup-and-run config set history_path PATH`.", "Rode o comando como root (Linux: `sudo`), ou aponte `history_path` para uma pasta gravável com `shutup-and-run config set history_path CAMINHO`."),
    ("ALLOWLIST_WARNING", "Add the image: `shutup-and-run config set allowlist IMAGE --apply`, or let the poll URL choose with `shutup-and-run config set poll_any_image true --apply`.", "Adicione a imagem: `shutup-and-run config set allowlist IMAGEM --apply`, ou deixe a poll URL escolher com `shutup-and-run config set poll_any_image true --apply`."),
    ("ALLOWLIST_FAILED", "Run `shutup-and-run config set allowlist IMAGE --apply` with at least one image.", "Rode `shutup-and-run config set allowlist IMAGEM --apply` com pelo menos uma imagem."),
    // Docker and the service.
    ("DOCKER_UNAVAILABLE", "Start Docker. Windows: `shutup-and-run wsl stop` then `shutup-and-run wsl start`. Linux: `sudo systemctl start docker`.", "Inicie o Docker. Windows: `shutup-and-run wsl stop` e depois `shutup-and-run wsl start`. Linux: `sudo systemctl start docker`."),
    ("DOCKER_FAILED", "Windows: `shutup-and-run wsl stop` then `shutup-and-run wsl start`; still failing, `shutup-and-run wsl reset`.", "Windows: `shutup-and-run wsl stop` e depois `shutup-and-run wsl start`; se continuar, `shutup-and-run wsl reset`."),
    ("SERVICE_PORT_IN_USE", "Pick another port: `shutup-and-run config set listen 127.0.0.1:8788`, then start the service again.", "Escolha outra porta: `shutup-and-run config set listen 127.0.0.1:8788` e inicie o serviço de novo."),
    ("SERVICE_FAILED", "Fix the docker line first, then start the service: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`.", "Corrija primeiro a linha do docker e inicie o serviço: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`."),
    ("SERVICE_NOT_RUNNING", "Start the service: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`.", "Inicie o serviço: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`."),
    ("WORKER_PAUSED", "Run `shutup-and-run resume`.", "Rode `shutup-and-run resume`."),
    ("LOCAL_ADDRESS_BANNED", "Run `shutup-and-run bans lift 127.0.0.1`, then check the token of whoever calls this machine.", "Rode `shutup-and-run bans lift 127.0.0.1` e confira o token de quem chama esta máquina."),
    ("ADDRESS_BANNED", "Only if one of the addresses is yours: `shutup-and-run bans lift IP`.", "Só se um dos endereços for seu: `shutup-and-run bans lift IP`."),
    ("RESOURCES_FAILED", "Free memory or disk on this machine, or lower the need: `shutup-and-run config set max_slots 1 --apply`.", "Libere memória ou disco nesta máquina, ou reduza a exigência: `shutup-and-run config set max_slots 1 --apply`."),
    ("RESOURCES_WARNING", "The worker waits until memory and disk are free again. Close heavy programs or free disk space.", "O worker espera até haver memória e disco livres. Feche programas pesados ou libere espaço em disco."),
    ("LOW_MEMORY", "Close heavy programs on this machine, then try again; or lower the floor: `shutup-and-run config set min_free_memory 1g --apply`.", "Feche programas pesados nesta máquina e tente de novo; ou reduza o mínimo: `shutup-and-run config set min_free_memory 1g --apply`."),
    ("LOW_DISK", "Free disk space: `docker image prune -af` (on Windows inside the distribution: `wsl -d shutup-and-run -u root docker image prune -af`).", "Libere espaço em disco: `docker image prune -af` (no Windows, dentro da distribuição: `wsl -d shutup-and-run -u root docker image prune -af`)."),
    ("WORKER_BUSY", "Every slot is in use. Wait for the running job (`shutup-and-run jobs`) and try again.", "Todos os slots estão em uso. Aguarde a tarefa em execução (`shutup-and-run jobs`) e tente de novo."),
    ("SLOTS_EXCEED_MEMORY", "Run `shutup-and-run config set max_slots 1 --apply`, or lower `max_job_memory`.", "Rode `shutup-and-run config set max_slots 1 --apply`, ou reduza `max_job_memory`."),
    // The poll URL.
    ("POLL_URL_UNSET", "Run `shutup-and-run config set poll_url URL --apply` with the URL the service gave you.", "Rode `shutup-and-run config set poll_url URL --apply` com a URL que o serviço informou."),
    ("POLL_UNREACHABLE", "Check the internet and the address: `shutup-and-run config get poll_url --reveal`. Then `shutup-and-run doctor` again.", "Confira a internet e o endereço: `shutup-and-run config get poll_url --reveal`. Depois rode `shutup-and-run doctor` de novo."),
    ("POLL_NOT_APPROVED", "Nothing to do on this machine: ask the service to approve this worker. It keeps asking by itself.", "Nada a fazer nesta máquina: peça ao serviço para aprovar este worker. Ele continua tentando sozinho."),
    ("POLL_HTTP_ERROR", "The poll URL answered with an error. Check the path and the query: `shutup-and-run config get poll_url --reveal`; fix with `shutup-and-run config set poll_url URL --apply`.", "A poll URL respondeu com erro. Confira o caminho e os parâmetros: `shutup-and-run config get poll_url --reveal`; corrija com `shutup-and-run config set poll_url URL --apply`."),
    ("POLL_RATE_LIMITED", "The poll URL asked this machine to slow down (429). Wait: the worker backs off by itself.", "A poll URL pediu para esta máquina ir mais devagar (429). Aguarde: o worker reduz o ritmo sozinho."),
    ("POLL_ERROR", "Check the URL and its query with `shutup-and-run config get poll_url --reveal`, then `shutup-and-run status`.", "Confira a URL e seus parâmetros com `shutup-and-run config get poll_url --reveal` e depois `shutup-and-run status`."),
    // job validate.
    ("JOB_INVALID_JSON", "Fix the JSON of the job file and run `shutup-and-run job validate FILE` again.", "Corrija o JSON do arquivo da tarefa e rode `shutup-and-run job validate ARQUIVO` de novo."),
    ("JOB_INVALID_FIELD", "Fix the field named in the error; docs/INTEGRATION-CONTRACT.md lists the fields of a job.", "Corrija o campo citado no erro; docs/INTEGRATION-CONTRACT.md lista os campos de uma tarefa."),
    ("JOB_IMAGE_NOT_ALLOWED", "Add the image: `shutup-and-run config set allowlist IMAGE --apply`, or validate as a polled task with `--poll`.", "Adicione a imagem: `shutup-and-run config set allowlist IMAGEM --apply`, ou valide como tarefa da poll URL com `--poll`."),
    // demo.
    ("DEMO_IMAGE_NOT_ALLOWED", "Add the demo image: `shutup-and-run config set allowlist hello-world --apply`.", "Adicione a imagem da demo: `shutup-and-run config set allowlist hello-world --apply`."),
    ("DEMO_JOB_INVALID", "Run `shutup-and-run config check` and fix what it reports.", "Rode `shutup-and-run config check` e corrija o que ele apontar."),
    ("JOB_FAILED", "Run `shutup-and-run config check` and fix what it reports.", "Rode `shutup-and-run config check` e corrija o que ele apontar."),
    ("ADMISSION_FAILED", "The machine is short of memory or disk, or paused. Run `shutup-and-run status` and free what it names.", "A máquina está com pouca memória ou disco, ou pausada. Rode `shutup-and-run status` e libere o que ele citar."),
    ("DEMO_TIMEOUT", "The demo container did not finish in time. Check the internet (the image is downloaded) and run the demo again.", "O container da demo não terminou a tempo. Confira a internet (a imagem é baixada) e rode a demo de novo."),
    ("CONTAINER_FAILED", "Run `shutup-and-run doctor`; if Docker is fine, run the demo again.", "Rode `shutup-and-run doctor`; se o Docker estiver bem, rode a demo de novo."),
    ("DEMO_IMAGE_UNAVAILABLE", "The image could not be downloaded. Check the internet and run the demo again.", "A imagem não pôde ser baixada. Confira a internet e rode a demo de novo."),
    ("IMAGE_FAILED", "The image could not be downloaded. Check the internet and run the demo again.", "A imagem não pôde ser baixada. Confira a internet e rode a demo de novo."),
    ("DEMO_JOB_FAILED", "The demo container exited with an error. Run `shutup-and-run doctor`, then the demo again.", "O container da demo terminou com erro. Rode `shutup-and-run doctor` e depois a demo de novo."),
    ("LIMITS_NOT_APPLIED", "Docker did not apply the limits. Update Docker, or on Windows run `shutup-and-run wsl reset`.", "O Docker não aplicou os limites. Atualize o Docker ou, no Windows, rode `shutup-and-run wsl reset`."),
    ("LIMITS_NOT_READ", "The limits could not be read back. Run the demo again; if it repeats, `shutup-and-run doctor --report`.", "Não foi possível ler os limites. Rode a demo de novo; se repetir, `shutup-and-run doctor --report`."),
    ("LIMITS_FAILED", "Docker did not apply the limits. Update Docker, or on Windows run `shutup-and-run wsl reset`.", "O Docker não aplicou os limites. Atualize o Docker ou, no Windows, rode `shutup-and-run wsl reset`."),
    ("DEMO_CLEANUP_FAILED", "A demo container was left behind. Remove it: `docker rm -f` the container named in the detail.", "Um container da demo ficou para trás. Remova: `docker rm -f` no container citado no detalhe."),
    ("CLEANUP_FAILED", "A demo container was left behind. Remove it: `docker rm -f` the container named in the detail.", "Um container da demo ficou para trás. Remova: `docker rm -f` no container citado no detalhe."),
    ("DEMO_WORKSPACE", "The demo could not create its temporary folder. Run it as root (Linux: `sudo`) and check free disk.", "A demo não conseguiu criar sua pasta temporária. Rode como root (Linux: `sudo`) e confira o disco livre."),
    ("WORKSPACE_FAILED", "The demo could not create its temporary folder. Run it as root (Linux: `sudo`) and check free disk.", "A demo não conseguiu criar sua pasta temporária. Rode como root (Linux: `sudo`) e confira o disco livre."),
    ("SIGNATURE_INVALID", "The signed PDF did not verify. Run `shutup-and-run certificate status` and the demo again.", "O PDF assinado não foi validado. Rode `shutup-and-run certificate status` e a demo de novo."),
    ("SIGNATURE_FAILED", "The signed PDF did not verify. Run `shutup-and-run certificate status` and the demo again.", "O PDF assinado não foi validado. Rode `shutup-and-run certificate status` e a demo de novo."),
    ("OUTPUT_NOT_WRITTEN", "Choose a folder you can write to with `--out FILE` and run the demo again.", "Escolha uma pasta onde você pode gravar com `--out ARQUIVO` e rode a demo de novo."),
    ("OUTPUT_FAILED", "Choose a folder you can write to with `--out FILE` and run the demo again.", "Escolha uma pasta onde você pode gravar com `--out ARQUIVO` e rode a demo de novo."),
    // The optional local certificate.
    ("CONSENT_REQUIRED", "Read the risk in the error. To accept it, repeat the command with `--accept-risk`.", "Leia o risco no erro. Para aceitar, repita o comando com `--accept-risk`."),
    ("CERTIFICATE_MISSING", "The local certificate is optional. To use one: `shutup-and-run certificate set --file FILE.pfx --accept-risk`; to stop asking for it: `shutup-and-run certificate disable`.", "O certificado local é opcional. Para usar um: `shutup-and-run certificate set --file ARQUIVO.pfx --accept-risk`; para não usar: `shutup-and-run certificate disable`."),
    ("CERTIFICATE_INVALID", "The file is not a readable .pfx/.p12. Export the certificate again with its private key and repeat `shutup-and-run certificate set`.", "O arquivo não é um .pfx/.p12 legível. Exporte o certificado de novo com a chave privada e repita `shutup-and-run certificate set`."),
    ("CERTIFICATE_PASSWORD_WRONG", "Wrong certificate password. Repeat `shutup-and-run certificate set` with the right one.", "Senha do certificado errada. Repita `shutup-and-run certificate set` com a senha certa."),
    ("CERTIFICATE_NO_PRIVATE_KEY", "Export the certificate again including the private key (.pfx) and repeat `shutup-and-run certificate set`.", "Exporte o certificado de novo incluindo a chave privada (.pfx) e repita `shutup-and-run certificate set`."),
    ("CERTIFICATE_EXPIRED", "The certificate expired. Get a new one and run `shutup-and-run certificate set` with it, or `shutup-and-run certificate disable`.", "O certificado venceu. Obtenha um novo e rode `shutup-and-run certificate set` com ele, ou `shutup-and-run certificate disable`."),
    ("CERTIFICATE_FAILED", "Run `shutup-and-run certificate status`; replace the certificate with `shutup-and-run certificate set`, or `shutup-and-run certificate disable`.", "Rode `shutup-and-run certificate status`; troque o certificado com `shutup-and-run certificate set`, ou `shutup-and-run certificate disable`."),
    ("CERTIFICATE_WARNING", "Run `shutup-and-run certificate status`; replace the certificate with `shutup-and-run certificate set`, or `shutup-and-run certificate disable`.", "Rode `shutup-and-run certificate status`; troque o certificado com `shutup-and-run certificate set`, ou `shutup-and-run certificate disable`."),
    ("CERTIFICATE_MOUNT_WRITABLE", "The container could write to the certificate folder. Run `shutup-and-run doctor --report` and open the issue link.", "O container conseguiu gravar na pasta do certificado. Rode `shutup-and-run doctor --report` e abra o link de issue."),
    ("MOUNT_FAILED", "The certificate was not visible in the container. Run `shutup-and-run certificate status` and the demo again.", "O certificado não apareceu no container. Rode `shutup-and-run certificate status` e a demo de novo."),
    ("OPENSSL_MISSING", "Install openssl (Linux: `sudo apt-get install -y openssl`); on Windows run `shutup-and-run wsl reset`.", "Instale o openssl (Linux: `sudo apt-get install -y openssl`); no Windows rode `shutup-and-run wsl reset`."),
    // Windows.
    ("WSL_UNAVAILABLE", "In an administrator PowerShell run `wsl --install --no-distribution`, restart the PC, then `shutup-and-run wsl install`.", "Em um PowerShell de administrador rode `wsl --install --no-distribution`, reinicie o PC e depois `shutup-and-run wsl install`."),
    ("WSL_FAILED", "In an administrator PowerShell run `wsl --update`, restart the PC, then `shutup-and-run doctor`.", "Em um PowerShell de administrador rode `wsl --update`, reinicie o PC e depois `shutup-and-run doctor`."),
    ("DISTRIBUTION_MISSING", "Run `shutup-and-run wsl install`.", "Rode `shutup-and-run wsl install`."),
    ("DISTRIBUTION_FAILED", "Run `shutup-and-run wsl reset` to rebuild the distribution; the config is kept.", "Rode `shutup-and-run wsl reset` para reconstruir a distribuição; a config é mantida."),
    ("BINARY_FAILED", "Run `shutup-and-run wsl install` again: it puts the right binary in the distribution.", "Rode `shutup-and-run wsl install` de novo: ele coloca o binário certo na distribuição."),
    ("TASK_NOT_REGISTERED", "Run `shutup-and-run wsl task register` so the worker starts at logon.", "Rode `shutup-and-run wsl task register` para o worker iniciar no logon."),
    ("TASK_WARNING", "Run `shutup-and-run wsl task register` so the worker starts at logon.", "Rode `shutup-and-run wsl task register` para o worker iniciar no logon."),
    ("WSL_LIMITS_WARNING", "Run `shutup-and-run wsl limits` to see what WSL2 may take; `shutup-and-run wsl limits --apply` sets the budget.", "Rode `shutup-and-run wsl limits` para ver quanto o WSL2 pode usar; `shutup-and-run wsl limits --apply` define o limite."),
    ("WSL_LIMITS_ABOVE_BUDGET", "Run `shutup-and-run wsl limits --apply`, then `wsl --shutdown` and `shutup-and-run wsl start`.", "Rode `shutup-and-run wsl limits --apply`, depois `wsl --shutdown` e `shutup-and-run wsl start`."),
    ("WSL_LIMITS_NOT_IN_EFFECT", "Restart WSL so the limits apply: `shutup-and-run wsl stop`, `wsl --shutdown`, `shutup-and-run wsl start`.", "Reinicie o WSL para os limites valerem: `shutup-and-run wsl stop`, `wsl --shutdown`, `shutup-and-run wsl start`."),
];

pub fn hint_in(code: &str, lang: Lang) -> &'static str {
    let (en, pt) = CATALOG
        .iter()
        .find(|(known, _, _)| *known == code)
        .map(|(_, en, pt)| (*en, *pt))
        .unwrap_or(GENERIC);
    match lang {
        Lang::En => en,
        Lang::Pt => pt,
    }
}

/// The hint of a code in the configured language. Never empty.
pub fn hint(code: &str) -> &'static str {
    hint_in(code, lang())
}

/// The last step before a JSON answer is printed: a failure gets a `code`
/// when it has none (from the exit code) and a `hint`.
pub fn attach(json: &mut Value, exit_code: u8) {
    if json.get("ok") != Some(&Value::Bool(false)) || json.get("hint").is_some() {
        return;
    }
    let code = match json.get("code").and_then(Value::as_str) {
        Some(code) if code != "OK" => code.to_string(),
        _ => match exit_code {
            crate::cli::EXIT_USAGE => "USAGE",
            crate::cli::EXIT_INVALID => "INVALID_VALUE",
            _ => "FAILED",
        }
        .to_string(),
    };
    json["hint"] = Value::String(hint(&code).to_string());
    json["code"] = Value::String(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every string in the sources shaped like a code: `"SOME_CODE"`. That
    /// is wider than the codes, so a code built in any way is still found;
    /// the few other names of that shape are listed here.
    fn codes_in(source: &str) -> Vec<String> {
        const NOT_CODES: &[&str] = &["DEMO_PDF_BEGIN", "DEMO_PDF_END", "DEMO_SCRIPT"];
        source
            .split('"')
            .filter(|word| {
                word.contains('_')
                    && word.starts_with(|c: char| c.is_ascii_uppercase())
                    && word
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                    && !word.starts_with("SHUTUP_AND_RUN_")
                    && !word.starts_with("CARGO_")
                    && !NOT_CODES.contains(word)
            })
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn no_error_or_warning_code_is_without_a_hint_in_both_languages() {
        let sources = [
            include_str!("cli.rs"),
            include_str!("devx.rs"),
            include_str!("wsl.rs"),
            include_str!("certificate.rs"),
            include_str!("jobs.rs"),
        ];
        let mut codes: Vec<String> = sources.iter().flat_map(|s| codes_in(s)).collect();
        codes.sort();
        codes.dedup();
        assert!(codes.len() > 40, "the scan found {} codes", codes.len());
        for code in [
            "DOCKER_UNAVAILABLE",
            "CERTIFICATE_PASSWORD_WRONG",
            "JOB_INVALID_JSON",
        ] {
            assert!(codes.iter().any(|c| c == code), "the scan missed {code}");
        }
        let fine = |code: &str| code.ends_with("_OK") || code.ends_with("_VALID") || code == "OK";
        for code in codes.iter().filter(|c| !fine(c)) {
            assert!(
                CATALOG.iter().any(|(known, _, _)| known == code),
                "{code} has no hint in src/hints.rs"
            );
        }
        // Each entry once, both languages filled in and different.
        for (i, (code, en, pt)) in CATALOG.iter().enumerate() {
            assert!(!en.is_empty() && !pt.is_empty() && en.len() < 260, "{code}");
            assert!(
                !CATALOG[..i].iter().any(|(other, _, _)| other == code),
                "{code} twice"
            );
        }
        assert!(hint_in("WORKER_PAUSED", Lang::Pt).starts_with("Rode"));
        assert!(hint_in("WORKER_PAUSED", Lang::En).starts_with("Run"));
        // A code nobody listed still tells the person what to do.
        assert!(!hint_in("SOMETHING_NEW", Lang::En).is_empty());
        assert!(hint_in("SOMETHING_NEW", Lang::Pt).contains("doctor --report"));
    }

    #[test]
    fn a_failure_gets_a_code_and_a_hint_and_a_success_gets_neither() {
        let mut usage = serde_json::json!({"ok": false, "error": "unknown option --x"});
        attach(&mut usage, crate::cli::EXIT_USAGE);
        assert_eq!(usage["code"], "USAGE");
        assert!(usage["hint"].as_str().unwrap().contains("--help"));

        let mut coded = serde_json::json!({"ok": false, "code": "JOB_INVALID_JSON"});
        attach(&mut coded, crate::cli::EXIT_INVALID);
        assert_eq!(coded["code"], "JOB_INVALID_JSON");
        assert!(coded["hint"].as_str().unwrap().contains("job validate"));

        let mut fine = serde_json::json!({"ok": true});
        attach(&mut fine, 0);
        assert!(fine.get("hint").is_none() && fine.get("code").is_none());
    }

    /// FOR-AGENTS.md is where an agent reads the catalogue.
    #[test]
    fn the_guide_for_agents_lists_every_code() {
        let guide = include_str!("../docs/FOR-AGENTS.md");
        for (code, _, _) in CATALOG {
            assert!(
                guide.contains(&format!("`{code}`")),
                "{code} is not in docs/FOR-AGENTS.md"
            );
        }
    }
}
