# Painel web do fork — Plano de Implementação

> **Para o Claude:** use `subagent-driven-development` ou `executing-plans` para implementar tarefa por
> tarefa. Um subagente por tarefa, revisão entre tarefas.

**Spec:** [`docs/alfama/specs/2026-09-24-painel-web-alfama-design.md`](../specs/2026-09-24-painel-web-alfama-design.md)

**Objetivo:** quatro telas novas na `/web` do ai-memory — Briefing, Linha do tempo, Propostas e Entre
projetos — mais a correção das datas do backfill, num fork que diverge do original o mínimo possível.

**Arquitetura:** consultas só leitura novas em `ai-memory-store/src/painel.rs`; o renderer do briefing sai
de `ai-memory-hooks` (privado) para `ai-memory-store/src/brief.rs`, e hooks e web chamam a mesma função;
rotas e templates askama novos em `ai-memory-web`; a data original do evento viaja do backfill até
`ops.rs` por um campo opcional `occurred_at`; um comando offline `repair-backfill-timestamps` corrige as
sessões já importadas. Três fases, um PR cada no fork.

**Stack:** Rust 1.95 (pinado em `rust-toolchain.toml`), axum 0.8, askama 0.12, rusqlite (sync) +
`ReaderPool::with_conn` (async via `spawn_blocking`), Tailwind CSS versionado, Docker.

---

## Antes de começar: fatos que o plano assume (levantados em 2026-09-24 no HEAD `39cd3dc5`)

1. **Não há Rust no Windows.** Todo `cargo` roda num container. Definição usada no plano inteiro como
   `CARGO <args>`, a partir da raiz do fork (`C:\Users\Maxsuel Einstein\Local Sites\ai-memory`, Git Bash):

   Uma vez, transferir os volumes de cache para uid 1000 (rodar como root faz `chmod` virar no-op, o que
   falseia 2 testes de logging do `ai-memory-cli`; ver item 2):

   ```bash
   MSYS_NO_PATHCONV=1 docker run --rm -v ai-memory-cargo:/c -v ai-memory-rustup:/r -v ai-memory-target:/t \
     rust:1.95 chown -R 1000:1000 /c /r /t
   ```

   ```bash
   CARGO() { MSYS_NO_PATHCONV=1 docker run --rm --user 1000:1000 -e HOME=/tmp/h \
     -e CARGO_HOME=/usr/local/cargo -e RUSTUP_HOME=/usr/local/rustup -v "$PWD:/w" -w /w \
     -v ai-memory-cargo:/usr/local/cargo/registry -v ai-memory-rustup:/usr/local/rustup \
     -v ai-memory-target:/w/target rust:1.95 bash -c "mkdir -p /tmp/h && cargo $*"; }
   ```

   O `target` fica em `/w/target` (dentro da arvore montada em `/w`), sem `CARGO_TARGET_DIR`: os testes
   de integracao do `ai-memory-cli` localizam o diretorio `hooks/` do repo subindo 3 niveis a partir do
   caminho do binario de teste, e um `CARGO_TARGET_DIR=/target` fora da arvore (`/w`) quebra essa conta.

   Os volumes nomeados guardam registry, toolchain e `target` entre execuções (compilar no bind mount do
   Windows é lento). `bash -c`, não `-lc`: o shell de login do Debian tira `/usr/local/cargo/bin` do PATH.
   `rust-toolchain.toml` já instala rustfmt e clippy. Baseline medido (Tarefa 0, 2026-09-24): hooks 308,
   store 488 (+1 ignorado de propósito), web 114, tudo verde, ~3,5 min com cache.
2. **Gate antes de cada PR** (AGENTS.md), em duas partes — workspace sem o `cli`, depois o `cli` isolado:
   `CARGO fmt --all -- --check`; `git diff --check`;
   `CARGO clippy --workspace --all-targets -- -D warnings`;
   `CARGO test --workspace --all-targets --no-fail-fast --exclude ai-memory-cli`; e, separado,
   `CARGO test -p ai-memory-cli --all-targets -- --test-threads=1`. Motivo, em uma linha: root ignora
   `chmod` (falseia testes de logging do `cli`) e os testes de hooks do `cli` ficam instáveis sob
   paralelismo da suíte inteira no bind mount do Windows — a CI do GitHub é a juíza final, não este gate
   local.
3. **Testes** ficam na `tests/suite/` de cada crate, declarados no `mod.rs` dela (compilam no harness da
   lib; **nenhum binário novo**). Web: `crates/ai-memory-web/tests/suite/routes.rs` tem `setup()`,
   `new_page(..)`, `seed_session(..)`, `handoff_for(..)`, `api_req(..)`. Store: cada arquivo abre
   `Store::open(tmp.path())` com `tempfile::TempDir`.
4. **Datas** no banco são `INTEGER` em microssegundos Unix; IDs são `BLOB`. `kind` de página não é coluna:
   é `page_kind_expr("pg.path", "pg.frontmatter_json")` (`reader.rs`).
5. **Links nos templates são relativos** (`w/…`, `static/…`): `mount.rs::inject_web_base_href` injeta
   `<base href>`. Nunca começar link com `/`.
6. **Escopo nas rotas:** `lookup_existing_scope(&state.reader, ws, proj)` (`store/src/scope.rs:253`). A
   página de projeto atual NÃO resolve escopo (projeto inexistente = 200 vazio); as telas novas resolvem e
   respondem 404.
7. **Tailwind:** classe nova em template exige `TAILWIND_BUILD=1` (Tarefa 6) e versionar
   `crates/ai-memory-web/static/tailwind.css` — o CI faz `git diff --exit-code` nele.
8. **Invariante 16:** páginas compartilhadas; `OwnerFilter` só em handoffs.

---

## Fase 1 — Base + Briefing (PR 1)

### Tarefa 0: Ambiente do clone

**Passo 1: Finais de linha e caminhos longos**

```bash
cd "/c/Users/Maxsuel Einstein/Local Sites/ai-memory"
git config core.longpaths true
git config core.autocrlf false
git status --porcelain            # tem de estar vazio antes de renormalizar
git rm -r -q --cached . && git reset -q --hard HEAD
grep -c $'\r' docker/Dockerfile bin/release scripts/install-git-hooks.sh
```

Esperado: `0` nos três. Se `git status` não estiver vazio, **pare** — não descarte trabalho.

**Passo 2: Baseline no container**

Run: `CARGO test -p ai-memory-web -p ai-memory-store -p ai-memory-hooks`
Esperado: tudo verde. É a referência; se algo já falhar na `v2.4.0`, relate e não siga.

**Passo 3: Workflows de publicação desligados** (sem mudar arquivo — nada a conflitar nos merges)

Com OK do usuário (é ação no repositório dele):

```bash
for w in release.yml windows.yml macos-app.yml nix.yml secret-scan.yml; do
  gh workflow disable "$w" -R maxeinstein-dev/ai-memory; done
gh workflow list -R maxeinstein-dev/ai-memory
```

Esperado: só `ci` ativo. (Em fork, o Actions pode estar desabilitado até o dono habilitar na aba
Actions — se estiver, habilitar e rodar o loop acima.)

---

### Tarefa 1: Renderer do briefing em `ai-memory-store`

**Arquivos:**
- Criar: `crates/ai-memory-store/src/brief.rs`
- Modificar: `crates/ai-memory-store/src/lib.rs` (declaração `mod brief;` junto dos outros `mod`, l.17-34, e
  `pub mod`/re-export)
- Modificar: `crates/ai-memory-hooks/src/router.rs` (remover o que foi movido; importar do store)
- Teste: `crates/ai-memory-store/tests/suite/brief.rs` (+ `mod brief;` em `tests/suite/mod.rs`)

**Passo 1: Mover, sem mudar comportamento**

De `crates/ai-memory-hooks/src/router.rs` para `brief.rs`, **verbatim** (mesmo corpo, mesmos comentários):

- funções: `render_session_brief` (l.1712), `render_brief_omitted_section` (l.1613),
  `render_brief_recent_section` (l.1658), `escape_untrusted_history_tail`, `truncate_at_char_boundary`,
  `brief_tail_len`;
- constantes: `BRIEF_BUDGET_DEFAULT` (4_000), `BRIEF_BUDGET_MIN` (1_500), `BRIEF_BUDGET_MAX` (20_000),
  `BRIEF_CORE_PAGES_LIMIT` (24), `BRIEF_RECENT_PAGES_LIMIT` (10), `BRIEF_PREAMBLE_TITLE`,
  `BRIEF_PREAMBLE_BOUNDARY`, `UNTRUSTED_HISTORY_START`, `UNTRUSTED_HISTORY_END`, `BRIEF_AGENT_FOOTER`,
  `BRIEF_OMITTED_HEADER`, `BRIEF_RECENT_HEADER`, `BRIEF_TRUNCATED_NOTICE`.

Visibilidade em `brief.rs`: `pub` para `render_session_brief`, os cinco `BRIEF_BUDGET_*`/`*_LIMIT`,
`escape_untrusted_history_tail`, `UNTRUSTED_HISTORY_START`/`END` (hooks ainda os usa em
`render_handoff_markdown`); o resto fica privado ao módulo. Troque `ai_memory_store::BriefPageBody` por
`crate::BriefPageBody` (idem `BriefingPage`). `ai_memory_core::UNTRUSTED_MEMORY_NOTICE` continua vindo do core.

Acrescente o clamp como função (hoje inline em `render_requested_session_brief`, l.1513-1518), para web e
hook não o duplicarem:

```rust
/// Orçamento efetivo do briefing: o pedido (se houver e for número), senão o padrão, sempre dentro de
/// [`BRIEF_BUDGET_MIN`, `BRIEF_BUDGET_MAX`]. Web e hook usam esta função — a prévia da web só é
/// idêntica ao que o hook injeta se os dois clamparem igual.
#[must_use]
pub fn clamp_brief_budget(requested: Option<&str>) -> usize {
    requested
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(BRIEF_BUDGET_DEFAULT)
        .clamp(BRIEF_BUDGET_MIN, BRIEF_BUDGET_MAX)
}
```

Em `lib.rs`: `pub mod brief;` (módulo público; os chamadores usam `ai_memory_store::brief::…`).

**Passo 2: Hooks passa a usar o store**

Em `router.rs`: remova os itens movidos; adicione
`use ai_memory_store::brief::{self, BRIEF_CORE_PAGES_LIMIT, BRIEF_RECENT_PAGES_LIMIT, UNTRUSTED_HISTORY_END,
UNTRUSTED_HISTORY_START, escape_untrusted_history_tail, render_session_brief};` (ajuste ao que o compilador
pedir); em `render_requested_session_brief` troque o bloco do clamp por
`let budget = brief::clamp_brief_budget(query.briefing_budget.as_deref());`.

**Passo 3: Mover os testes do renderer**

Os testes `render_session_brief_*` (hooks `router.rs:10897-11013`) e o de fronteira de confiança perto da
l.4061 que só exercitam o renderer vão para `crates/ai-memory-store/tests/suite/brief.rs`, chamando
`ai_memory_store::brief::render_session_brief`. Testes que exercitam o endpoint do hook ficam em hooks.
Acrescente:

```rust
use ai_memory_store::brief::{clamp_brief_budget, BRIEF_BUDGET_DEFAULT, BRIEF_BUDGET_MAX, BRIEF_BUDGET_MIN};

#[test]
fn clamp_usa_padrao_sem_pedido_e_respeita_limites() {
    assert_eq!(clamp_brief_budget(None), BRIEF_BUDGET_DEFAULT);
    assert_eq!(clamp_brief_budget(Some("abc")), BRIEF_BUDGET_DEFAULT);
    assert_eq!(clamp_brief_budget(Some("10")), BRIEF_BUDGET_MIN);
    assert_eq!(clamp_brief_budget(Some("999999")), BRIEF_BUDGET_MAX);
    assert_eq!(clamp_brief_budget(Some(" 6000 ")), 6000);
}
```

**Passo 4: Rodar**

Run: `CARGO test -p ai-memory-store -p ai-memory-hooks`
Esperado: verde, com o mesmo número de testes de antes (movidos, não perdidos) + 1.
Run: `CARGO clippy -p ai-memory-store -p ai-memory-hooks --all-targets -- -D warnings` → sem avisos.

**Passo 5: Commit**

```bash
git add crates/ai-memory-store crates/ai-memory-hooks
git commit -m "refactor(brief): renderer do briefing sai de hooks para ai-memory-store::brief"
```

---

### Tarefa 2: Utilidades comuns da web e abas

**Arquivos:**
- Modificar: `crates/ai-memory-web/src/routes/mod.rs` (helpers `pub(crate)`)
- Modificar: `crates/ai-memory-web/src/routes/page.rs` (usar o `not_found_response` movido)
- Criar: `crates/ai-memory-web/templates/_abas.html`
- Modificar: `crates/ai-memory-web/templates/project.html` (incluir as abas entre o `<h1>` l.14 e o
  `<div class="flex …">` l.16)
- Modificar: `crates/ai-memory-web/src/templates.rs` (campo `aba` em `ProjectView`)
- Teste: `crates/ai-memory-web/tests/suite/routes.rs`

**Passo 1: Teste que falha**

```rust
#[tokio::test]
async fn pagina_de_projeto_mostra_as_abas_do_painel() {
    let (_tmp, store, wiki) = setup().await;
    let ws = store.writer.get_or_create_workspace("default").await.unwrap();
    let _ = store.writer.get_or_create_project(ws, "scratch", None).await.unwrap();
    let app = router(store.reader.clone(), wiki.clone());
    let resp = app.oneshot(Request::builder().uri("/w/default/scratch").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    for href in ["w/default/scratch/briefing", "w/default/scratch/linha-do-tempo", "w/default/scratch/propostas"] {
        assert!(text.contains(href), "falta a aba {href}");
    }
}
```

Run: `CARGO test -p ai-memory-web pagina_de_projeto_mostra_as_abas` → FAIL.

**Passo 2: Implementar**

Em `routes/mod.rs`, mova `not_found_response` de `page.rs:143` para cá como `pub(crate)` e acrescente:

```rust
use ai_memory_core::{ProjectId, WorkspaceId};
use ai_memory_store::{lookup_existing_scope, ScopeResolutionError};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Escopo de uma tela HTML: 404 HTML para workspace/projeto inexistente, 500 para falha do store.
pub(crate) async fn escopo_html(
    state: &WebState, workspace: &str, project: &str,
) -> Result<(WorkspaceId, ProjectId), Response> {
    lookup_existing_scope(&state.reader, workspace, project)
        .await
        .map(ai_memory_store::ResolvedScope::as_tuple)
        .map_err(|err| {
            if err.is_not_found() { not_found_response() } else {
                tracing::error!(error = %err, "resolvendo escopo da tela do painel");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        })
}
```

(Confira os caminhos de import `ai_memory_core::{ProjectId, WorkspaceId}` contra os usados em
`routes/api.rs`; use os mesmos.)

`templates/_abas.html` (usa só classes já presentes em `project.html` — reaproveite as do `<nav>` do
breadcrumb; se precisar de classe nova, ela entra na Tarefa 6):

```html
<nav class="mb-6 flex items-center gap-4 text-sm border-b pb-2">
  <a href="{{ base_href }}" class="{% if aba == "paginas" %}font-semibold{% endif %}">Páginas</a>
  <a href="{{ base_href }}/briefing" class="{% if aba == "briefing" %}font-semibold{% endif %}">Briefing</a>
  <a href="{{ base_href }}/linha-do-tempo" class="{% if aba == "linha" %}font-semibold{% endif %}">Linha do tempo</a>
  <a href="{{ base_href }}/propostas" class="{% if aba == "propostas" %}font-semibold{% endif %}">Propostas</a>
</nav>
```

Todo template que inclui `_abas.html` tem os campos `base_href: String` (= `project_href(ws, proj)`) e
`aba: &'static str`. Em `ProjectView` acrescente os dois e preencha em `routes/project.rs`
(`base_href: project_href(&workspace, &project), aba: "paginas"`). Em `project.html`,
`{% include "_abas.html" %}` na posição indicada.

**Passo 3: Rodar**

Run: `CARGO test -p ai-memory-web` → verde (inclusive os testes existentes de `page.rs`).

**Passo 4: Commit**

```bash
git add crates/ai-memory-web
git commit -m "feat(web): abas do painel na pagina de projeto e escopo HTML com 404"
```

---

### Tarefa 3: Tela de Briefing

**Arquivos:**
- Criar: `crates/ai-memory-web/src/routes/painel_briefing.rs`
- Criar: `crates/ai-memory-web/templates/painel_briefing.html`
- Modificar: `crates/ai-memory-web/src/templates.rs` (`BriefingView`), `src/routes/mod.rs` (rota + `mod`)
- Teste: `crates/ai-memory-web/tests/suite/routes.rs`

**Passo 1: Testes que falham**

```rust
#[tokio::test]
async fn briefing_mostra_o_mesmo_texto_que_o_renderer_do_hook() {
    let (_tmp, store, wiki) = setup().await;
    let ws = store.writer.get_or_create_workspace("default").await.unwrap();
    let proj = store.writer.get_or_create_project(ws, "scratch", None).await.unwrap();
    store.writer.upsert_page(new_page(ws, proj, "_rules/nunca-x.md", "Nunca faça X", "Nunca faça X, porque Y.")).await.unwrap();

    // Esperado calculado pelo MESMO caminho do hook: mesmas páginas, mesmo renderer, mesmo clamp.
    let (core, recent) = store.reader.session_brief_pages_with_slot_visibility(
        ws, proj,
        ai_memory_store::brief::BRIEF_CORE_PAGES_LIMIT,
        ai_memory_store::brief::BRIEF_RECENT_PAGES_LIMIT,
        ai_memory_core::SlotVisibility::All,
    ).await.unwrap();
    let esperado = ai_memory_store::brief::render_session_brief(
        &core, &recent, ai_memory_store::brief::clamp_brief_budget(None)).unwrap();

    let visto = ai_memory_web::montar_briefing_para_teste(&store.reader, ws, proj, None).await.unwrap();
    assert_eq!(visto.markdown, esperado, "a prévia divergiu do que o hook injeta");
    assert!(visto.markdown.contains("Nunca faça X"));
}

#[tokio::test]
async fn briefing_de_projeto_inexistente_responde_404() {
    let (_tmp, store, wiki) = setup().await;
    let app = router(store.reader.clone(), wiki.clone());
    let resp = app.oneshot(Request::builder().uri("/w/default/nao-existe/briefing").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn briefing_clampa_max_chars_como_o_servidor() {
    let (_tmp, store, wiki) = setup().await;
    let ws = store.writer.get_or_create_workspace("default").await.unwrap();
    let _ = store.writer.get_or_create_project(ws, "scratch", None).await.unwrap();
    let app = router(store.reader.clone(), wiki.clone());
    let resp = app.oneshot(Request::builder().uri("/w/default/scratch/briefing?max_chars=10").body(Body::empty()).unwrap()).await.unwrap();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert!(std::str::from_utf8(&body).unwrap().contains("1500"), "orçamento mínimo não apareceu");
}
```

`montar_briefing_para_teste` é uma função `#[doc(hidden)] pub` em `lib.rs` que só repassa para a função
interna da rota (o teste precisa do markdown cru, que o HTML escapa). Run: `CARGO test -p ai-memory-web
briefing` → FAIL (não compila).

**Passo 2: Implementar**

`src/routes/painel_briefing.rs`:

```rust
//! Tela de Briefing: o texto exato que o hook injeta no início da sessão, com o uso do orçamento.
use std::sync::Arc;

use ai_memory_core::{ProjectId, SlotVisibility, WorkspaceId};
use ai_memory_store::brief::{self, BRIEF_CORE_PAGES_LIMIT, BRIEF_RECENT_PAGES_LIMIT};
use ai_memory_store::ReaderPool;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::state::WebState;
use crate::templates::{project_href, BriefingView, ItemDoBriefing};

#[derive(Deserialize)]
pub(crate) struct Params { max_chars: Option<String> }

pub(crate) struct Briefing {
    pub markdown: String,
    pub orcamento: usize,
    pub itens: Vec<ItemDoBriefing>,
}

/// Mesmas páginas, mesmo renderer e mesmo clamp do hook. `SlotVisibility::All`: a web já lista todas
/// as páginas do projeto (invariante 16); com `[slots] per_user = true` a prévia mostra a união dos
/// slots, e a tela diz isso.
pub(crate) async fn montar(
    reader: &ReaderPool, ws: WorkspaceId, proj: ProjectId, max_chars: Option<&str>,
) -> anyhow::Result<Briefing> {
    let orcamento = brief::clamp_brief_budget(max_chars);
    let (core, recent) = reader
        .session_brief_pages_with_slot_visibility(ws, proj, BRIEF_CORE_PAGES_LIMIT, BRIEF_RECENT_PAGES_LIMIT, SlotVisibility::All)
        .await?;
    let markdown = brief::render_session_brief(&core, &recent, orcamento).unwrap_or_default();
    let itens = core.iter().map(|p| ItemDoBriefing {
        path: p.path.clone(), title: p.title.clone(), chars: p.body.chars().count(),
        entrou: markdown.contains(&p.title),
    }).collect();
    Ok(Briefing { markdown, orcamento, itens })
}

pub(crate) async fn handler(
    State(state): State<Arc<WebState>>,
    Path((workspace, project)): Path<(String, String)>,
    Query(params): Query<Params>,
) -> Response {
    let (ws, proj) = match crate::routes::escopo_html(&state, &workspace, &project).await {
        Ok(s) => s, Err(resp) => return resp,
    };
    let b = match montar(&state.reader, ws, proj, params.max_chars.as_deref()).await {
        Ok(b) => b,
        Err(e) => { tracing::error!(error = %e, "montando briefing"); return StatusCode::INTERNAL_SERVER_ERROR.into_response(); }
    };
    let usados = b.markdown.chars().count();
    let view = BriefingView {
        base_href: project_href(&workspace, &project), aba: "briefing",
        workspace, project, usados, orcamento: b.orcamento,
        pct: (usados * 100 / b.orcamento.max(1)).min(100),
        html: crate::markdown::render(&b.markdown), markdown: b.markdown, itens: b.itens,
    };
    match view.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
```

(`crate::markdown::render`: use a função que `routes/page.rs` já usa para renderizar Markdown — confira o
nome exato em `src/markdown.rs`. `entrou` por título é aproximação: a seção de omitidas do próprio
renderer é a fonte da verdade e aparece no markdown.)

Em `templates.rs`:

```rust
pub(crate) struct ItemDoBriefing { pub path: String, pub title: String, pub chars: usize, pub entrou: bool }

#[derive(Template)]
#[template(path = "painel_briefing.html")]
pub(crate) struct BriefingView {
    pub workspace: String, pub project: String, pub base_href: String, pub aba: &'static str,
    pub usados: usize, pub orcamento: usize, pub pct: usize,
    pub html: String, pub markdown: String, pub itens: Vec<ItemDoBriefing>,
}
```

`templates/painel_briefing.html` (copie o `<nav>` de breadcrumb e o `<h1>` de `project.html`):

```html
{% extends "base.html" %}
{% block title %}Briefing · {{ workspace }}/{{ project }}{% endblock %}
{% block content %}
<h1 class="text-2xl font-semibold mb-4">{{ workspace }}/{{ project }}</h1>
{% include "_abas.html" %}
<p class="text-sm mb-2">{{ usados }} de {{ orcamento }} caracteres ({{ pct }}%) — é o texto que o agente recebe no início de cada sessão. Simule outro limite com <code>?max_chars=</code>.</p>
<div class="w-full h-2 bg-gray-200 rounded mb-6"><div class="h-2 bg-gray-700 rounded" style="width: {{ pct }}%"></div></div>
<h2 class="text-lg font-semibold mb-2">Como o agente vê</h2>
<article class="prose max-w-none mb-8">{{ html|safe }}</article>
<details class="mb-8"><summary>Markdown cru</summary><pre class="whitespace-pre-wrap text-xs">{{ markdown }}</pre></details>
<h2 class="text-lg font-semibold mb-2">Páginas centrais consideradas</h2>
<ul class="text-sm">
{% for i in itens %}<li>{% if i.entrou %}✓{% else %}✗{% endif %} {{ i.title }} <span class="text-gray-500">{{ i.path }} · {{ i.chars }} caracteres</span></li>{% endfor %}
</ul>
{% endblock %}
```

(O `style="width: …"` inline evita classe Tailwind dinâmica. `|safe` só no HTML que saiu do renderer de
Markdown do próprio crate, o mesmo uso de `page.html`.)

Em `routes/mod.rs`: `mod painel_briefing;` e
`.route("/w/{workspace}/{project}/briefing", get(painel_briefing::handler))`.
Em `lib.rs`: o `montar_briefing_para_teste` que chama `routes::painel_briefing::montar`.

**Passo 3: Rodar**

Run: `CARGO test -p ai-memory-web` → verde.

**Passo 4: Commit**

```bash
git add crates/ai-memory-web
git commit -m "feat(web): tela de Briefing -- o texto exato do inicio de sessao e o uso do orcamento"
```

---

### Tarefa 4: Tailwind, gate e imagem

**Passo 1: CSS** — se algum template novo usa classe que não existia, no layout não-root da função `CARGO`
do fato 1 (`target` dentro da árvore, em `/w/target`, sem `CARGO_TARGET_DIR` fora dela — um
`CARGO_TARGET_DIR=/target` externo quebra a localização de `hooks/` pelos testes do `cli`), com
`TAILWIND_BUILD=1` só nesta chamada:

```bash
MSYS_NO_PATHCONV=1 docker run --rm --user 1000:1000 -e HOME=/tmp/h \
  -e CARGO_HOME=/usr/local/cargo -e RUSTUP_HOME=/usr/local/rustup -e TAILWIND_BUILD=1 \
  -v "$PWD:/w" -w /w -v ai-memory-cargo:/usr/local/cargo/registry -v ai-memory-rustup:/usr/local/rustup \
  -v ai-memory-target:/w/target rust:1.95 bash -c "mkdir -p /tmp/h && cargo build -p ai-memory-web"
```

e commitar `crates/ai-memory-web/static/tailwind.css`. Conferir `git diff --stat` (só o CSS muda).

**Nota (bind mount do Windows, confirmada em 2026-09-25 na Tarefa 8):** o comando acima falha nesta
árvore — o `build.rs` tenta um `std::fs::copy` de volta para `static/tailwind.css` e o bind mount do
Windows recusa copiar permissões (`PermissionDenied: Operation not permitted`), deixando o arquivo
**vazio**. O CSS novo, porém, já foi gerado no `OUT_DIR` do build antes da cópia falhar. Contorno: rodar o
build (que falha, sem problema) e depois copiar o arquivo gerado com `cat` de **dentro** do container, sem
passar pelo `fs::copy` do host:

```bash
MSYS_NO_PATHCONV=1 docker run --rm --user 1000:1000 -e HOME=/tmp/h \
  -e CARGO_HOME=/usr/local/cargo -e RUSTUP_HOME=/usr/local/rustup \
  -v "$PWD:/w" -w /w -v ai-memory-cargo:/usr/local/cargo/registry -v ai-memory-rustup:/usr/local/rustup \
  -v ai-memory-target:/w/target rust:1.95 bash -c '
f=$(ls -t /w/target/debug/build/ai-memory-web-*/out/tailwind.css | head -1)
cat "$f" > crates/ai-memory-web/static/tailwind.css
'
```

Depois, conferir `git diff --exit-code -- crates/ai-memory-web/static/tailwind.css` só ADICIONA as classes
esperadas (não usar grep para decidir se precisa regenerar: qualquer classe nova no template exige rodar
isto de novo, é o `git diff --exit-code` do CI que julga). Se a cópia sair vazia ou corrompida,
`git checkout -- crates/ai-memory-web/static/tailwind.css` desfaz e tenta de novo. A PR da linha do tempo
já tinha falhado uma vez na CI por causa disso (classes `w-20`, `list-disc`, `pl-5` fora do CSS
versionado).

**Passo 2: Gate** (fato 2). Tudo verde.

**Passo 3: Imagem**

```bash
docker build -f docker/Dockerfile --target runtime-source -t ai-memory-alfama:2.4.0-alfama.1 .
```

**Passo 4: Trocar a imagem local** (com OK do usuário — é o servidor de memória em uso)

Em `C:\Users\Maxsuel Einstein\ai-memory\docker-compose.yml`, `image: ai-memory-alfama:2.4.0-alfama.1`
(comentário: fork, qual PR). Antes: `docker exec ai-memory ai-memory --data-dir /data backup`. Depois:
`docker compose up -d`, `status` e `/mcp` como antes, e `http://127.0.0.1:49374/web/w/default/aw-senai-sgw/briefing`
abrindo.

**Passo 5: Verificação 3 da spec** — **NÃO** chame `GET /handoff`: ele consome o handoff pendente de
qualquer sessão (spec §2.1). Compare o "Markdown cru" da tela com o bloco de briefing de um início de sessão
real, ou só chame o endpoint depois de `memory_handoff_list` vazio para o projeto. Feito em 2026-09-25:
contido integralmente (3.846 caracteres).

**Passo 6: PR 1 no fork** (com OK do usuário): push de `alfama/main` numa branch
`alfama/fase-1-briefing` e PR para `alfama/main` no fork (`gh pr create -R maxeinstein-dev/ai-memory
--base alfama/main`). CI `ci` tem de ficar verde.

---

## Fase 2 — Propostas + Linha do tempo + datas (três PRs, um assunto cada)

> Decisão do dono (2026-09-25, §2.2 da spec): branches por **tipo de assunto**, não por fase, e um
> assunto por PR. A Fase 2 deixa de ser um PR só e vira três: `alfama/feat-linha-do-tempo` (Tarefas 5 e
> 6, gate na Tarefa 7), `alfama/feat-propostas` (Tarefa 8, gate na Tarefa 9), e
> `alfama/fix-backfill-timestamps` (Tarefas 10 e 11, reparo real e gate na Tarefa 12). O conteúdo técnico
> das tarefas não muda — só onde cada uma commita/vira PR.

### Tarefa 5: Consultas da linha do tempo

**Arquivos:**
- Criar: `crates/ai-memory-store/src/painel.rs` (`impl ReaderPool` com `with_conn`, como `auto_improve.rs`)
- Modificar: `crates/ai-memory-store/src/lib.rs` (`mod painel;` + `pub use painel::{SessaoNaLinha, PaginaProduzida};`)
- Teste: `crates/ai-memory-store/tests/suite/painel.rs` (+ `mod painel;`)

**Passo 0: Confirmar o formato de `page_evidence.source_id`** para `source_kind = 'session'`:
`grep -rn "page_evidence" crates/ai-memory-store/src/ | grep -i insert` — ver se grava o UUID com hífens
(`SessionId::to_string()`) ou outro formato; a consulta abaixo assume `to_string()`. Ajustar se diferente.

**Passo 1: Teste que falha**

```rust
#[tokio::test]
async fn linha_do_tempo_lista_sessoes_e_o_que_cada_uma_produziu() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store.writer.get_or_create_workspace("default").await.unwrap();
    let proj = store.writer.get_or_create_project(ws, "p", None).await.unwrap();
    // uma sessão encerrada + uma página com evidência dela (siga o helper seed_session de
    // crates/ai-memory-web/tests/suite/routes.rs:1079 e o insert de evidência que o Passo 0 achou)
    let sid = /* begin_session + end_session */;
    /* upsert_page("gotchas/x.md") + registrar evidência (page, 'session', sid) */;

    let linha = store.reader.linha_do_tempo(ws, proj, 0).await.unwrap();
    assert_eq!(linha.len(), 1);
    assert_eq!(linha[0].id, sid.to_string());
    assert_eq!(linha[0].produziu.len(), 1);
    assert_eq!(linha[0].produziu[0].path, "gotchas/x.md");
}
```

(Os `/* */` são preenchidos com as chamadas reais do writer encontradas no Passo 0 — o executor não
inventa API.) Run: `CARGO test -p ai-memory-store linha_do_tempo` → FAIL.

**Passo 2: Implementar** em `painel.rs`:

```rust
//! Consultas só leitura das telas do painel (fork). Nada aqui escreve.
use ai_memory_core::{ProjectId, WorkspaceId};
use rusqlite::params;

use crate::reader::{page_kind_expr, ReaderPool};
use crate::StoreResult;

#[derive(Debug, Clone)]
pub struct PaginaProduzida { pub path: String, pub title: String, pub kind: String }

#[derive(Debug, Clone)]
pub struct SessaoNaLinha {
    pub id: String, pub agent: String,
    pub started_us: i64, pub ended_us: Option<i64>, pub observacoes: i64,
    pub produziu: Vec<PaginaProduzida>,
}

impl ReaderPool {
    /// Sessões do projeto desde `desde_us` (mais recentes primeiro, até 500) e as páginas atuais que
    /// têm evidência de cada uma.
    pub async fn linha_do_tempo(&self, ws: WorkspaceId, proj: ProjectId, desde_us: i64) -> StoreResult<Vec<SessaoNaLinha>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, agent_kind, started_at, ended_at, ended_observation_count FROM sessions \
                 WHERE workspace_id = ?1 AND project_id = ?2 AND started_at >= ?3 \
                 ORDER BY started_at DESC LIMIT 500")?;
            let mut out: Vec<SessaoNaLinha> = stmt.query_map(params![ws.as_bytes(), proj.as_bytes(), desde_us], |r| {
                let id: Vec<u8> = r.get(0)?;
                Ok(SessaoNaLinha {
                    id: uuid::Uuid::from_slice(&id).map(|u| u.to_string()).unwrap_or_default(),
                    agent: r.get(1)?, started_us: r.get(2)?, ended_us: r.get(3)?, observacoes: r.get(4)?,
                    produziu: Vec::new(),
                })
            })?.collect::<Result<_, _>>()?;
            let kind = page_kind_expr("pg.path", "pg.frontmatter_json");
            let mut ev = conn.prepare(&format!(
                "SELECT pe.source_id, pg.path, pg.title, {kind} FROM page_evidence pe \
                 JOIN pages pg ON pg.id = pe.page_id \
                 WHERE pe.source_kind = 'session' AND pg.workspace_id = ?1 AND pg.project_id = ?2 \
                 AND pg.is_latest = 1 ORDER BY pg.path"))?;
            let rows = ev.query_map(params![ws.as_bytes(), proj.as_bytes()], |r| {
                Ok((r.get::<_, String>(0)?, PaginaProduzida { path: r.get(1)?, title: r.get(2)?, kind: r.get(3)? }))
            })?;
            for row in rows {
                let (sid, pagina) = row?;
                if let Some(s) = out.iter_mut().find(|s| s.id == sid) { s.produziu.push(pagina); }
            }
            Ok(out)
        }).await
    }
}
```

(Confira: `page_kind_expr` e `ReaderPool` são `pub(crate)`/`pub` em `reader.rs` — se `page_kind_expr` for
privado, torne-o `pub(crate)`; `uuid` já é dependência do store; a conversão de BLOB para id deve seguir o
helper que `reader.rs` já usa para `SessionId` — prefira-o ao `Uuid::from_slice` se existir.)

**Passo 3: Rodar** `CARGO test -p ai-memory-store painel` → verde. **Commit**
`feat(store): consulta da linha do tempo com as paginas que cada sessao produziu`.

**Passo 4: Teste adversarial (regra do `AGENTS.md` do `main` do original, §2.2 da spec)** —
`linha_do_tempo` é um ponto de entrada que lê por `(workspace_id, project_id)`; culpado até um teste provar
que não vaza. **Já implementado nesta tarefa** (HEAD `d9d2cee7`): `pagina_de_outro_projeto_nao_entra_em_produziu`
prova que uma página de outro projeto não aparece em `produziu`. **Falta acrescentar** (registrado em
2026-09-25, decisão do dono): um teste de que uma **sessão** de outro projeto não aparece na linha do tempo
deste projeto, mesmo com o id da sessão válido — o que existe cobre só o lado das páginas, não o das
sessões. Formato: semear a sessão no projeto B, chamar `linha_do_tempo` no projeto A e afirmar que ela não
aparece (controle: a mesma sessão aparece na linha do tempo do projeto B); rodar uma vez com o filtro de
`project_id` removido da query de `sessions` para confirmar que o teste morde, depois restaurar o filtro.
Atualizar a linha `alfama-2` de `docs/security-boundaries.md` (FUTURE → STRONG) no mesmo commit/PR desse
teste.

---

### Tarefa 6: Tela da Linha do tempo

**Arquivos:** `src/routes/painel_linha.rs`, `templates/painel_linha.html`, `templates.rs` (`LinhaView`,
`DiaNaLinha`), `routes/mod.rs` (rota `/w/{workspace}/{project}/linha-do-tempo`), teste em `routes.rs`.

**Passo 1: Testes que falham** — (a) projeto com uma sessão semeada (`seed_session`) responde 200 e o HTML
contém o agente e a data do dia; (b) `?dias=7` e `?dias=90` aceitos, `?dias=5` cai no padrão 30 (o HTML
mostra "últimos 30 dias"); (c) projeto inexistente → 404.

**Passo 2: Implementar** — handler no molde do de Briefing: `escopo_html`, `dias ∈ {7,30,90}` senão 30,
`desde_us = now_us - dias*86_400_000_000`, `reader.linha_do_tempo(ws, proj, desde_us)`, agrupar por dia
(`jiff::Timestamp::from_microsecond(started_us)` → data no fuso UTC; o fuso local fica para depois, e a
tela diz "UTC"), calcular a barra de cada dia como `% = sessões_do_dia * 100 / max_sessões_no_período`
(largura via `style`, sem classe dinâmica). Template: barras (uma `<div>` por dia com `style="width: N%"`),
depois a lista por dia com agente, duração (`ended - started`, "em aberto" se `None`), nº de observações e
a lista `produziu` com link `w/{ws}/{proj}/p/{path}` (use `page_href`).

**Passo 3/4:** `CARGO test -p ai-memory-web` verde; commit
`feat(web): tela da Linha do tempo -- sessoes por dia e o que cada uma produziu`.

**Passo 5: Teste adversarial** — a rota também é um ponto de entrada por `(workspace, project)` cru na URL.
Estender o teste (c) do Passo 1 (projeto inexistente → 404) com um caso de **projeto existente, mas de
outro workspace**, para provar que `escopo_html` recusa também esse cruzamento (controle: o mesmo projeto,
no workspace certo, responde 200); rodar uma vez trocando `lookup_existing_scope` por uma resolução só por
`project` (ignorando `workspace`) para confirmar que o teste morde, depois desfazer. Atualizar a linha
`alfama-2` de `docs/security-boundaries.md` para citar também este teste de rota (além do de `painel.rs` da
Tarefa 5).

---

### Tarefa 7: Gate, imagem e PR — `alfama/feat-linha-do-tempo`

1. Tailwind (Passo 1 da Tarefa 4, se algum template novo usa classe nova), gate completo (fato 2).
2. Imagem `ai-memory-alfama:2.4.0-alfama.2`; troca no compose (backup antes, como no Passo 4 da Tarefa 4).
3. `CHANGELOG.md` `[Unreleased]`: entrada em `### Added` para a tela Linha do tempo (mesmo PR).
4. PR no fork a partir de uma branch `alfama/feat-linha-do-tempo` para `alfama/main`
   (`gh pr create -R maxeinstein-dev/ai-memory --base alfama/main`), corpo pelo
   `.github/pull_request_template.md` + checklist de segurança da spec §2.1. Conferir autoria antes do
   push (§2.2 da spec). CI `ci` verde.

---

### Tarefa 8: Tela de Propostas

**Arquivos:** `src/routes/painel_propostas.rs`, `templates/painel_propostas.html`, `templates.rs`,
`routes/mod.rs` (rota `/w/{workspace}/{project}/propostas`), teste em `routes.rs`; `painel.rs` só se a
Passo 0 mostrar que falta dado.

**Passo 0:** conferir em `crates/ai-memory-store/src/auto_improve.rs:259` se `AutoImproveProposalDetail`
expõe `target_body_sha256_at_stage` e `body_markdown`. Se o hash de estágio não estiver no tipo, acrescente
em `painel.rs` `pub async fn hash_de_estagio_da_proposta(&self, ws, proj, id) -> StoreResult<Option<Vec<u8>>>`
(SELECT `target_body_sha256_at_stage` de `auto_improve_proposals`).

**Passo 1: Testes que falham** — semear uma proposta pendente (siga como os testes existentes do store
criam `auto_improve_runs`/`auto_improve_proposals`: `grep -rn "stage_auto_improve\|insert_auto_improve" crates/ai-memory-store/src`
e reuse a API do writer) e verificar: (a) 200 com título, `target_path`, confiança e o texto exato
`ai-memory pending-writes approve <ID>`; (b) `?status=approved` não mostra a pendente; (c) proposta de
`update` cuja página alvo mudou depois do estágio mostra "conflito"; (d) projeto inexistente → 404.

**Passo 2: Implementar** — `status` via `AutoImproveProposalStatus::from_str` (inválido → `pending`);
`reader.list_auto_improve_proposals(ws, proj, Some(status), 100)`; para cada uma,
`auto_improve_proposal_detail(ws, proj, id)` e, se `operation == update`, `page_body_by_ids(ws, proj,
target_path)` para o "antes"; conflito = `sha256(corpo_atual) != target_body_sha256_at_stage` (use o mesmo
helper de hash que o store usa para `body_sha256` — `sha2` já está no crate web). Template: cartão por
proposta com cabeçalho (título, kind, operação, alvo, confiança em %, "conflito" em destaque), justificativa,
evidências (links para as sessões quando o `evidence_json` citar sessão), **antes | depois** em duas colunas
(`<pre>` cada; `md:flex-row` se já existir no CSS), e os dois comandos num `<pre>` selecionável. Sem
botões.

**Passo 3/4:** verde; commit `feat(web): tela de Propostas -- pendentes com antes/depois e o comando pronto`.

**Passo 5: Teste adversarial** — listagem e detalhe são pontos de entrada por `(workspace, project, id)`
com um id cru de proposta na URL/rota. Acrescentar: (a) uma proposta pendente do projeto B não aparece na
listagem `?status=pending` do projeto A (controle: aparece na listagem do projeto B); (b) o **detalhe**
dessa proposta, acessado pelo id dela a partir da rota do projeto A, responde 404 — não vaza corpo nem
metadado de uma proposta alheia mesmo com o id certo. Rodar uma vez sem o filtro de `project_id` na
consulta de `list_auto_improve_proposals`/`auto_improve_proposal_detail` para confirmar que o teste morde,
depois restaurar o filtro. Atualizar a linha `alfama-3` de `docs/security-boundaries.md` (FUTURE → STRONG)
no mesmo commit/PR.

---

### Tarefa 9: Gate, imagem e PR — `alfama/feat-propostas`

1. Tailwind (se preciso), gate completo.
2. Imagem `ai-memory-alfama:2.4.0-alfama.3`; troca no compose (backup antes).
3. `CHANGELOG.md` `[Unreleased]`: entrada em `### Added` para a tela Propostas (mesmo PR).
4. PR no fork a partir de `alfama/feat-propostas` para `alfama/main`, corpo pelo template + checklist de
   segurança, autoria conferida antes do push. CI verde.

---

### Tarefa 10: `occurred_at` do backfill até o banco

**Arquivos:**
- `crates/ai-memory-core/src/observation.rs` (`NewSession` l.140, `NewObservation` l.85)
- `crates/ai-memory-store/src/ops.rs` (l.1465, l.1582, l.1902)
- `crates/ai-memory-hooks/src/payload.rs` (`HookEnvelope` l.98) e o ponto do router que monta
  `NewSession`/`NewObservation` a partir do envelope
- `crates/ai-memory-cli/src/commands/backfill.rs` (`map_event` l.414, `hook_item` l.470)
- Testes: store (`tests/suite/`), hooks (payload), cli (backfill)

**Passo 1: Testes que falham**

- store: `begin_session` com `occurred_at: Some(t)` grava `started_at = t`; `end_session` idem para
  `ended_at`; `insert_observation` idem para `created_at`; com `None`, continua "agora" (±5 s).
- hooks: um envelope JSON com `"occurred_at": "2026-09-10T12:00:00Z"` desserializa para
  `Some(1_789_… µs)`; string inválida → `None` (nunca erro — o hook é fire-and-forget).
- cli: `map_event` de um evento com `occurred_at` põe o campo no body.

**Passo 2: Implementar**

- core: `pub occurred_at: Option<i64>` (µs) em `NewSession` e `NewObservation`, com doc "hora original do
  evento; `None` = agora". **Todo literal desses structs no workspace** precisa do campo:
  `grep -rn "NewSession {" crates/ ; grep -rn "NewObservation {" crates/` e acrescente `occurred_at: None`
  (os de backfill recebem o valor).
- ops.rs: nas três linhas, `let now = x.occurred_at.unwrap_or_else(|| Timestamp::now().as_microsecond());`
  (para `end_session_row`, o valor chega pela assinatura — acrescente o parâmetro opcional ao comando do
  writer que a chama).
- hooks: `#[serde(default)] pub occurred_at: Option<String>` no `HookEnvelope`; converter com
  `occurred_at.as_deref().and_then(|s| s.parse::<jiff::Timestamp>().ok()).map(|t| t.as_microsecond())` no
  ponto que monta `NewSession`/`NewObservation`. **Não** passa pelo sanitizador (é metadado numérico, não
  texto) — comente isso onde o valor é lido, citando o invariante 6.
- backfill: em `map_event`, `"occurred_at": event.occurred_at` no body (e no item de fim de sessão, o
  `occurred_at` do último evento). Evento sem `occurred_at` herda o do anterior (spec §4).

**Passo 3:** `CARGO test --workspace --all-targets` verde (muitos crates tocados); clippy sem avisos.
**Commit** `fix(backfill): a data original do evento chega ao banco (occurred_at opcional)`.

---

### Tarefa 11: `repair-backfill-timestamps`

**Arquivos:**
- `crates/ai-memory-cli/src/cli.rs` (subcomando + args), `src/commands/mod.rs`,
  `src/commands/repair_backfill_timestamps.rs` (novo)
- writer: um comando `set_session_times(session_id, started_us, ended_us)` no `WriterHandle` (siga o
  padrão de um comando existente simples do writer — `grep -n "pub async fn end_session" crates/ai-memory-store/src/`)
- Teste: `crates/ai-memory-cli/tests/…` (o CLI tem harness próprio — ver AGENTS.md: um arquivo novo em
  `tests/suite` precisa ser declarado; o teste de layout do repositório reprova arquivo solto)

**Passo 1: Testes que falham**

- `planejar_reparo(transcricoes: &Path, sessoes_no_banco: &[(String, i64, Option<i64>)])` (função pura)
  lê `.jsonl` de exemplo (primeira e última linha com `timestamp`) e devolve, por sessão casada, o par
  novo `(started, ended)`; id não-UUID casa pelo UUID v5 (`Uuid::new_v5(&Uuid::NAMESPACE_OID, raw)`, mesma
  regra de `resolve_native_session_id` em `hooks/src/router.rs:3064`); sessão sem transcrição fica de fora.
- o comando sem `--apply` não escreve (contagem antes = depois);
- com `--apply` e um processo irmão "vivo" é recusado com `busy_message` (em teste, o guard é pulado por
  `cfg!(test)` — teste a função de decisão, não o sysinfo).

**Passo 2: Implementar** — molde de `commands/reindex.rs:35-62`: `sibling_processes()` →
`bail!(busy_message("repair-backfill-timestamps", ..))` se houver; `Store::open(&config.data_dir)`;
descobrir as transcrições do projeto pelo mesmo caminho que o backfill usa (reuse a função de descoberta
de `backfill.rs`, tornando-a `pub(crate)` se preciso); `planejar_reparo`; imprimir o relatório (sessões
casadas, intervalo de datas antes → depois); com `--apply`, `store.writer.set_session_times(..)` por
sessão. `--help` diz: rode `ai-memory backup` antes, com o servidor parado.

**Passo 3:** verde; commit `feat(cli): repair-backfill-timestamps corrige inicio/fim de sessoes importadas`.

**Passo 4: Teste adversarial** — `repair-backfill-timestamps` é uma operação destrutiva sobre `sessions`
filtrada por `--project`; acrescentar um caso em que o banco tem sessões de **dois** projetos com o mesmo
padrão de transcrição casável, e o comando com `--project P` só reescreve as de `P` (controle: as do outro
projeto mantêm `started_at`/`ended_at` originais). Rodar uma vez sem o filtro de projeto em
`planejar_reparo`/na consulta que lista `sessoes_no_banco` para confirmar que o teste morde (o outro
projeto também seria reescrito), depois restaurar o filtro. Não há linha nova em
`docs/security-boundaries.md` para isto — é um guard de escopo dentro de uma operação já coberta pelo
requisito de "checagem de processo vivo" (tabela original, linha 10); citar o teste no PR.

---

### Tarefa 12: Gate, imagem, reparo real e PR — `alfama/fix-backfill-timestamps`

1. Tailwind (se preciso), gate completo.
2. Imagem `ai-memory-alfama:2.4.0-alfama.4`; troca no compose (backup antes).
3. **Reparo real** (com OK do usuário): `docker compose stop`; rodar o comando num container com o volume
   montado e as transcrições **somente leitura** (`-v "$HOME/.claude/projects:/transcricoes:ro"`), primeiro
   sem `--apply` (colar o relatório), depois com; `docker compose up -d`.
4. Verificação 4 da spec: a linha do tempo do SGW espalhada pelos dias reais.
5. `CHANGELOG.md` `[Unreleased]`: entrada em `### Fixed` para a correção das datas do backfill (mesmo PR).
6. PR no fork a partir de `alfama/fix-backfill-timestamps` para `alfama/main`, corpo pelo template +
   checklist de segurança, autoria conferida antes do push. CI verde.

---

## Fase 3 — Entre projetos (PR `alfama/feat-entre-projetos`)

### Tarefa 13: Agrupamento de regras

**Arquivos:** `painel.rs` (+ `RegraCandidata`, `GrupoDeRegras`), teste em `tests/suite/painel.rs`.

**Passo 1: Testes que falham** (função pura, sem banco):

```rust
#[test]
fn regras_parecidas_de_projetos_diferentes_se_agrupam() {
    use ai_memory_store::painel::{agrupar_regras, RegraCandidata};
    let r = |proj: &str, titulo: &str, v: Vec<f32>| RegraCandidata {
        projeto: proj.into(), path: format!("_rules/{titulo}.md"), titulo: titulo.into(), vetor: Some(v) };
    let grupos = agrupar_regras(vec![
        r("a", "Nunca commitar .env", vec![1.0, 0.0]),
        r("b", "Não versionar arquivos .env", vec![0.99, 0.05]),
        r("c", "Usar tabs", vec![0.0, 1.0]),
    ], 0.85);
    assert_eq!(grupos.len(), 1);
    assert_eq!(grupos[0].projetos(), vec!["a", "b"]);
}

#[test]
fn sem_vetor_agrupa_por_titulo_normalizado() { /* dois títulos iguais a menos de caixa/acentos/espaços, vetor None, em projetos diferentes -> 1 grupo */ }

#[test]
fn regras_do_mesmo_projeto_nao_formam_grupo_sozinhas() { /* duas parecidas no mesmo projeto -> 0 grupos */ }
```

**Passo 2: Implementar** — `pub fn agrupar_regras(regras: Vec<RegraCandidata>, limiar: f32) -> Vec<GrupoDeRegras>`:
cosseno entre pares com vetor (união simples: se ≥ limiar, mesmo grupo); sem vetor, chave = título em
minúsculas, sem acentos (tabela fixa pequena, como a do `NoLegacyDomainTest` do SGW — nada de crate nova) e
espaços colapsados; mantém só grupos com **≥ 2 projetos distintos**. E
`pub async fn regras_de_todos_os_projetos(&self) -> StoreResult<Vec<RegraCandidata>>`: páginas `is_latest`
de kind `rule` (via `page_kind_expr`) ou `path LIKE '_rules/%'`, com o vetor de `page_embeddings` **só**
se `(provider, model, dim)` for o trio mais comum no banco (invariante 8: vetor de outro modelo é
ignorado, fica `None`); vetor lido com `f32::from_le_bytes` em blocos de 4 (formato de `f32_vec_to_bytes`,
`reader.rs:9252`).

**Passo 3:** verde; commit `feat(store): regras candidatas a globais por similaridade local`.

**Passo 4: Teste adversarial** — `regras_de_todos_os_projetos` lê **entre** projetos por desenho (é o ponto
da tela); o que precisa de teste é que ela não devolve nada de um workspace diferente do escopo permitido
quando chamada de um contexto autenticado por workspace (se o produto não distinguir workspace aqui,
registrar isso como decisão explícita no PR, não como omissão). Acrescentar um caso com regras equivalentes
em dois workspaces diferentes e afirmar que só as candidatas visíveis ao ator aparecem (controle: dentro do
mesmo workspace, agrupam normalmente). Atualizar `docs/security-boundaries.md` (linha `alfama-4`, parte de
regras) com o resultado.

---

### Tarefa 14: Tela Entre projetos

**Arquivos:** `src/routes/painel_entre.rs`, `templates/painel_entre.html`, `templates.rs`,
`routes/mod.rs` (rota `/entre-projetos`), link no header de `base.html` (`<a href="entre-projetos">`),
teste em `routes.rs`.

**Passo 1: Testes que falham** — (a) duas regras equivalentes em dois projetos aparecem num grupo; (b) um
handoff aberto aparece; um aceito, não; (c) handoff de outro dono **não** aparece para um ator (use
`api_req_actor`/extensões `ActorContext` como os testes de handoff da API — `routes.rs:1703`,
`handoff_for(..)` l.1737); (d) mensagem `pending` aparece com origem → destino.

**Passo 2: Implementar** — o handler recebe `actor: Option<Extension<ActorContext>>` e
`auth: Option<Extension<AuthLevel>>`; `OwnerFilter` e a regra de redação do corpo iguais às da API:
torne `owner_filter_for` (`api.rs:559`) e `serves_handoff_body` (`api.rs:608`) `pub(crate)` e reuse —
**não** duplique. Para cada projeto (`reader` já lista projetos para a página inicial; reuse), 
`list_handoffs(ws, proj, Some(HandoffState::Open), owner_filter.clone(), 50)` e
`list_messages(ws, proj, MessageBox::Inbox /* confira o nome da variante */, 50)`; regras via
`regras_de_todos_os_projetos` + `agrupar_regras(.., 0.85)`. Template: três seções (Regras candidatas a
globais, Handoffs abertos, Mensagens pendentes), com "nada aqui" quando vazio.

**Passo 3/4:** verde; commit `feat(web): tela Entre projetos -- regras repetidas, handoffs e mensagens`.

**Passo 5: Teste adversarial** — o teste (c) do Passo 1 já cobre handoff de outro dono não aparecendo
(`OwnerFilter` reusado da API). Acrescentar o par que falta: **mensagens pendentes** de um par
origem→destino que não inclui o ator autenticado não aparecem na lista dele (controle: uma mensagem cujo
destino é o ator aparece); rodar uma vez chamando `list_messages` sem o filtro por caixa/ator para confirmar
que o teste morde, depois restaurar. Atualizar a linha `alfama-4` de `docs/security-boundaries.md`
(FUTURE → STRONG, citando os testes de handoff e de mensagem) no mesmo commit/PR.

---

### Tarefa 15: Gate, imagem e PR — `alfama/feat-entre-projetos`

1. Tailwind (se preciso), gate completo.
2. Imagem `ai-memory-alfama:2.4.0-alfama.5`; troca no compose (backup antes).
3. `CHANGELOG.md` `[Unreleased]`: entrada em `### Added` para a tela Entre projetos (mesmo PR).
4. Verificação 6 da spec.
5. PR no fork a partir de `alfama/feat-entre-projetos` para `alfama/main`, corpo pelo template + checklist
   de segurança, autoria conferida antes do push. CI verde.

---

## Depois das 3 fases

- Atualizar `C:\Users\Maxsuel Einstein\ai-memory\LEIA-ME.md`: imagem do fork, como rebuildar, como
  sincronizar com o original (rebase de `alfama/main` sobre a nova tag + gate).
- Registrar na memória do projeto que o servidor roda a imagem do fork.
