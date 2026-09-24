# A `/web` mostra o que foi guardado, mas não o que o agente recebe, o que aprendeu quando, nem o que espera decisão

> Spec do fork `maxeinstein-dev/ai-memory` (de [akitaonrails/ai-memory](https://github.com/akitaonrails/ai-memory),
> MIT). Branch de trabalho: `alfama/main`, sobre a tag `v2.4.0`. Documentos do fork vivem em
> `docs/alfama/`, fora do `docs/` do original, para não conflitar nos merges.
>
> Escopo pedido pelo usuário em 2026-09-24: as ideias 1 (briefing), 6 (linha do tempo) e 7 (entre
> projetos) da avaliação da `/web`, mais a fila de propostas pendentes do item 2.

## 1. O problema, medido (2026-09-24, instalação local com 101 sessões importadas)

### 1.1. O que a `/web` mostra hoje

`crates/ai-memory-web` (axum + askama + Tailwind, **só leitura** por desenho) tem cinco rotas: lista de
projetos, página de projeto (árvore de páginas + "Recent activity"), página, busca FTS5 e estáticos. A
página de projeto separa **PAGES** (páginas de conhecimento que a consolidação por LLM escreve) de
**SYSTEM** (resumos de sessão, logs, manifestos).

### 1.2. O que ela não mostra, e por que importa

- **O briefing.** No início de toda sessão o hook injeta `_rules` + `_slots` + recentes, cortados por
  `[briefing] max_chars`. É o conteúdo de maior peso sobre o comportamento do agente, e não há onde vê-lo
  como o agente o vê. O texto é montado por `render_session_brief`
  (`crates/ai-memory-hooks/src/router.rs:1712`), **privada** ao crate dos hooks.
- **Quando se aprendeu o quê.** `page_evidence` (V63) liga cada página às sessões de origem, mas nada
  expõe "o que a sessão X produziu" nem uma visão por dia.
- **O que espera decisão.** `auto_improve_proposals` (V21) guarda propostas com alvo, corpo, justificativa,
  confiança e evidências; hoje só `ai-memory pending-writes list|show|diff|approve|reject` as mostra.
- **O que se repete entre projetos.** Regras equivalentes em projetos diferentes são candidatas a regra
  global; `handoffs` (estado `open`) e `agent_messages` (estado `pending`) cruzam projetos. Nenhuma tela
  junta isso.

### 1.3. Um defeito medido que afeta a linha do tempo

**As 101 sessões e as 89.652 observações importadas por `ai-memory backfill` estão todas datadas do dia
do import** (`sessions.started_at` e `observations.created_at`, em microssegundos, agrupados por dia:
`[('2026-09-24', 101)]` e `[('2026-09-24', 89652)]`). As transcrições do Claude Code
(`~/.claude/projects/<pasta>/*.jsonl`) têm o `timestamp` original de cada evento — o backfill descarta.
Sem corrigir, a linha do tempo do histórico importado é uma barra só.

## 2. O desenho: divergência mínima, em arquivos novos

| Onde | O quê |
|---|---|
| `crates/ai-memory-store/src/painel.rs` (novo) | consultas só leitura das quatro telas |
| `crates/ai-memory-web/src/routes/painel/*.rs` + `templates/painel_*.html` (novos) | rotas e templates askama |
| `crates/ai-memory-store/src/brief.rs` (novo) | `render_session_brief` e as constantes/auxiliares dela **movidas** de `ai-memory-hooks/src/router.rs` (onde eram privadas) — o crate web não depende de hooks, e o store (de onde já vêm `BriefPageBody`/`BriefingPage`) é o ponto comum sem ciclo. Hooks e web chamam a MESMA função |
| `crates/ai-memory-cli`, `-hooks`, `-core`, `-store` | backfill leva o `occurred_at` do evento até o servidor (campo opcional no body do `/hook` → `HookEnvelope` → `NewSession`/`NewObservation` → os três `Timestamp::now()` de `ops.rs`); comando novo `repair-backfill-timestamps` |
| `crates/ai-memory-web/src/routes/mod.rs` | 4–5 linhas de registro de rota (o ponto de conflito esperado nos merges) |

Regras do `AGENTS.md` do projeto que o desenho respeita, e onde:

- **Escopo** só por `ScopeResolver` e helpers (`lookup_existing_scope`, `resolve_many_existing_scopes`) —
  nunca cadeia de lookup à mão.
- **Invariante 16:** páginas são compartilhadas; `OwnerFilter` vale **só para handoffs**. Briefing, linha
  do tempo, propostas e regras candidatas não filtram por dono; a lista de handoffs filtra, como a API
  (`routes/api.rs`) já faz.
- **Invariante 2 (escritor único) e 9 (checagem de processo vivo):** o `repair` escreve pelo
  `WriterHandle`, **offline**, no mesmo padrão do `reindex` (§5).
- **Sem superfície pública sem chamador:** a função tornada `pub` ganha chamador no mesmo PR.
- **Tailwind:** classe nova exige `TAILWIND_BUILD=1 cargo build -p ai-memory-web` e versionar
  `static/tailwind.css` (o CI confere). Reaproveitar classes já presentes sempre que der.
- **Idioma:** textos visíveis das telas novas em português (decisão do usuário em 2026-09-24); comentários
  e docs de código no idioma de cada arquivo (inglês), como pede o AGENTS.md.

Navegação: a página do projeto ganha abas **Páginas** (a atual) | **Briefing** | **Linha do tempo** |
**Propostas**; o cabeçalho ganha **Entre projetos**. Sem JavaScript novo.

## 3. As quatro telas

### 3.1. Briefing — `/w/{workspace}/{project}/briefing`

- Texto **exato** do início de sessão, gerado por `render_session_brief` (agora em `ai_memory_store::brief`)
  sobre as mesmas páginas que o hook seleciona (`session_brief_pages_with_slot_visibility`, com os mesmos
  limites de 24 páginas centrais e 10 recentes) — renderizado e em Markdown cru.
- Barra de uso: caracteres usados × orçamento. O `max_chars` efetivo mora no marcador do **cliente**; a
  tela usa o padrão do servidor e aceita `?max_chars=` para simular, com o **mesmo clamp** que o servidor
  aplica (`BRIEF_BUDGET_MIN`/máximo).
- Lista do que entrou (caminho, tipo, tamanho) e do que ficou de fora ou foi cortado (a seção de omitidas
  que `render_brief_omitted_section` já produz).

### 3.2. Linha do tempo — `/w/{workspace}/{project}/linha-do-tempo?dias=30`

- Sessões por dia (barras só com CSS) e lista agrupada por dia: agente, duração, nº de observações.
- **O que cada sessão produziu:** páginas criadas ou atualizadas por ela (`page_evidence`,
  `source_kind = 'session'`), com o `kind` (rule, decision, concept, gotcha…).
- `dias` aceita 7, 30, 90; padrão 30.

### 3.3. Propostas — `/w/{workspace}/{project}/propostas?status=pending`

- Filtro por `status` (`pending` por padrão; `approved`, `rejected`, `conflict`).
- Cada proposta: título, `kind`, operação (`create`/`update`), `target_path`, confiança, justificativa,
  sessões de evidência (links), corpo proposto renderizado e, para `update`, o **diff** contra a página
  atual. **Não é diff linha a linha**: o `pending-writes diff` do original também não é (`admin.rs:3030`
  concatena antes/depois com `format!`) e o workspace não tem crate de diff. A tela mostra **antes e depois
  lado a lado**, sem dependência nova.
- Aviso de **conflito** quando a página alvo mudou depois da proposta (`target_body_sha256_at_stage` ≠
  corpo atual).
- Comandos prontos para copiar: `ai-memory pending-writes approve <ID>` e `ai-memory pending-writes
  reject <ID>`. **A web não aprova nem rejeita** (§6).

### 3.4. Entre projetos — `/entre-projetos`

- **Regras candidatas a globais:** páginas `kind = rule` (e as de `_rules/`) de projetos diferentes que
  dizem a mesma coisa. Similaridade por cosseno sobre os **embeddings locais** já calculados
  (`page_embeddings`, `all-MiniLM-L6-v2`, sem nada sair da máquina), limiar **0,85**, respeitando o
  invariante 8 (vetor com `{provider, model, dim}` diferentes é ignorado). Sem embedding, cai para título
  normalizado igual. Cada grupo lista os projetos.
- **Handoffs abertos** (`state = 'open'`, com `OwnerFilter`) e **mensagens pendentes**
  (`state = 'pending'`): origem → destino, resumo, idade.

## 4. Correção das datas do backfill

- **Import novo:** o backfill passa a gravar em `sessions.started_at`/`ended_at` e em
  `observations.created_at` o `timestamp` de cada evento da transcrição; sem `timestamp` num evento, usa o
  do evento anterior (nunca a hora do import no meio de uma sessão datada).
- **Sessões já importadas:** `ai-memory repair-backfill-timestamps [--project P] [--apply]`
  - lê as transcrições locais e casa pelo id da sessão (o id nativo, ou o UUID v5 dele quando o nativo não é
    UUID — mesma regra de `resolve_native_session_id`);
  - corrige **`sessions.started_at`/`ended_at`** (primeiro e último `timestamp` da transcrição), que é o que a
    linha do tempo usa; **não** corrige `observations.created_at` das sessões antigas — não há como casar
    cada observação ao evento de origem com segurança (as de imports novos já nascem certas pelo item acima);
  - **dry-run por padrão** (relata quantas sessões/observações mudariam e o intervalo de datas);
  - com `--apply`: exige o servidor **parado** (checagem de processo vivo, como `reindex`), escreve pelo
    `WriterHandle`, numa transação por sessão;
  - não reimporta, não reconsolida, não toca em páginas — não gasta cota de LLM;
  - antes de rodar, o operador faz `ai-memory backup` (documentado no próprio `--help`).
- As duas partes são candidatas a PR no original (é defeito dele).

## 5. Testes

No padrão do projeto: arquivos em `tests/suite/` de cada crate, declarados no `mod.rs`, **sem binário
novo**; nenhum teste acima de ~1 s fora do tier `slow`.

- `ai-memory-web/tests/suite/routes.rs`: por tela, caso vazio, caso com dados, 404 de escopo inexistente,
  e handoffs com `OwnerFilter`.
- `ai-memory-store/tests/suite/`: cada consulta de `painel.rs` sobre banco temporário.
- **Prévia = hook:** para o mesmo projeto e orçamento, o texto da tela é idêntico ao que o caminho do hook
  produz. Quebra se alguém duplicar a montagem.
- **Datas:** transcrição `.jsonl` de exemplo prova que o import grava a data original e que o `repair`
  corrige uma sessão antiga sem mexer em páginas; `--apply` com servidor vivo é recusado.
- **Agrupamento:** embeddings de exemplo provam o limiar e o fallback por título; vetor de outro modelo é
  ignorado.

Gate antes de cada PR (dentro de um container `rust:1.95`, sem toolchain no Windows):
`cargo fmt --all -- --check`, `git diff --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo tf`.

## 6. Entrega

**Fases, um PR cada no fork:**

1. **Base + Briefing:** clone do fork com `core.longpaths true` (o repositório tem caminhos acima do
   limite do Windows; sem isso o checkout falha em `routing_skills/`) e `core.autocrlf false` (o Git for
   Windows vem com `true` na config de sistema, e o checkout medido em 2026-09-24 pôs CRLF no
   `docker/Dockerfile`, em `bin/release` e nos scripts `.sh` — que quebram quando o build os executa num
   container Linux; refazer o checkout depois de desligar), branch `alfama/main`, workflows de publicação
   desligados, build da imagem, troca da imagem no compose local, tela de Briefing (a menor, prova o
   caminho inteiro).
2. **Propostas + Linha do tempo + correção das datas.**
3. **Entre projetos.**

**Build:** `docker build -f docker/Dockerfile --target runtime-source -t ai-memory-alfama:2.4.0-alfama.N .`
O compose local (`~\ai-memory\docker-compose.yml`) troca só a imagem; o cliente `ai-memory.exe` continua o
oficial 2.4.0 (as mudanças são todas do servidor).

**CI no fork:** o fork é público numa conta pessoal, então o `ci.yml` herdado (testes, clippy, fmt, CSS)
fica ligado. `release.yml`, `windows.yml`, `macos-app.yml` e `nix.yml` ficam **desligados** — publicariam
artefatos com o nome do original.

**Sincronizar com o original:** a cada release, rebase de `alfama/main` sobre a nova tag + gate completo;
o conflito esperado é o registro de rotas em `routes/mod.rs`.

**Riscos:** o original anda rápido (2.0 → 2.4 em três semanas) — por isso arquivos novos; o `repair`
escreve no banco — dry-run, backup, servidor parado, escritor único; erro nas telas novas responde 500 só
naquela rota, sem afetar MCP, hooks ou a `/web` original.

## Verificação

1. Gate da §5 verde em cada PR; CI do fork verde.
2. Imagem `ai-memory-alfama` no compose local; `status` e `/mcp` respondendo como antes; hooks capturando
   (uma sessão nova aparece na linha do tempo).
3. **Briefing:** o texto da tela para `aw-senai-sgw` é idêntico ao `additionalContext` que o hook de início
   de sessão devolve para o mesmo projeto (comparação feita chamando o endpoint do hook com os mesmos
   parâmetros).
4. **Linha do tempo:** depois de `repair-backfill-timestamps --apply`, as sessões do SGW se espalham pelos
   dias reais das transcrições (não mais só 2026-09-24); as páginas do SGW aparecem sob as sessões que as
   geraram.
5. **Propostas:** uma proposta gerada por `ai-memory auto-improve --session-id …` aparece com diff e
   comando; aprovada pelo CLI, some da lista `pending`.
6. **Entre projetos:** uma regra equivalente plantada em dois projetos de teste aparece agrupada; os
   handoffs abertos batem com `ai-memory handoffs`.

## O que esta spec NÃO entrega

- Aprovar ou rejeitar propostas na web (a web continua só leitura; a decisão foi do usuário).
- Editar páginas, autenticação nova, mudanças no cliente `ai-memory.exe`.
- Os PRs para o projeto original (ficam preparáveis, não abertos).
- Layout mobile dedicado.
- Painéis de privacidade, cobertura de captura e uso de LLM (itens 3–5 da avaliação) — ficaram fora do
  escopo pedido.

## O que esta spec deliberadamente NÃO decide

- O limiar final de similaridade: nasce em 0,85 e se ajusta com dados reais.
- Publicar a imagem num registro (GHCR) em vez de compilar localmente.
- Transferir o fork para a organização alfamaweb.
- Se a correção das datas vai para o original antes ou depois de rodar no fork.
