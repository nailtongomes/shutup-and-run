# shutup-and-run

Binário estático, uma máquina, HTTPS na porta pública. Um `POST` autenticado sobe um container se houver slot livre (`shutup-and-run=1`). Sem slot, recusa. Sem fila, sem escalonador, sem interface. Toda rota exige o token. Três erros de autenticação do mesmo IP banem esse IP por 48 horas.

A documentação completa está em [README.md](README.md). Com `poll_url`, a máquina também pede trabalho. As convenções do operador estão em [docs/CONVENTIONS.md](docs/CONVENTIONS.md).

```bash
sudo SHUTUP_AND_RUN_BIN=./shutup-and-run bash scripts/install.sh
sudo bash scripts/accept.sh
```
