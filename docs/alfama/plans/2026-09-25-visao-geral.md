# Plano: Visão geral do projeto e Linha do tempo com rótulos

> Implementa [`../specs/2026-09-25-visao-geral-design.md`](../specs/2026-09-25-visao-geral-design.md).
> Branch `alfama/feat-visao-geral`, um PR no fork. Vale tudo do plano anterior
> ([`2026-09-24-painel-web-alfama.md`](2026-09-24-painel-web-alfama.md), "Antes de começar"): a função
> `CARGO`, o gate em duas partes (workspace sem o cli, depois o cli com `--test-threads=1`) e as regras de
> segurança da spec do painel (§2.1).

## Antes de começar (fatos do código em 2026-09-25, `alfama/main` `ace726cf`)

1. **Disco (incidente de 2026-09-25):** um build de Rust por vez, nunca dois containers em paralelo.
   Conferir o espaço livre do C: antes de cada build e parar abaixo de 15 GB. O volume de target do fork é
   `ai-memory-target`, recriado vazio e com dono uid 1000; não criar outros.
2. **Rota do projeto:** `GET /w/{workspace}/{project}` é `routes/project.rs::handler`, que mostra a árvore
   de páginas e o "Recent activity", monta `ProjectView { .., aba: "paginas" }` e usa `is_system_page`
   para separar conhecimento de maquinaria.
3. **Páginas centrais:** o briefing usa
   `ReaderPool::session_brief_pages_with_slot_visibility(ws, proj, BRIEF_CORE_PAGES_LIMIT,
   BRIEF_RECENT_PAGES_LIMIT, slot_visibility)`, que devolve `(core, recent)` do tipo `Vec<BriefPageBody>`
   com `path`, `title` e `body`. A Visão geral usa essa mesma chamada com `SlotVisibility::All`, como a tela
   de Briefing, e mostra os `core`.
4. **Evidência:** `page_evidence(page_id BLOB, source_kind TEXT, source_id TEXT, created_at)`. Para
   `source_kind = 'session'`, `source_id` é `SessionId::to_string()`, o UUID com hífens, enquanto
   `sessions.id` é BLOB. O casamento é feito em Rust: um mapa `id → started_at` das sessões **do mesmo
   workspace e projeto**, com uma evidência resolvida por `source_id.parse::<Uuid>()`. É o mesmo cuidado de
   `ReaderPool::timeline`.
5. **Tipo e resumo:** o tipo vem de `page_kind_expr` (`pub(crate)` em `painel.rs`). O resumo vem de
   `frontmatter_json.summary`; sem ele, do primeiro parágrafo do corpo sem título nem frontmatter, cortado
   em 200 caracteres por limite de `char`.
6. **Tailwind:** o `build.rs` não consegue gravar o CSS na montagem do Windows e deixa o arquivo vazio. O
   contorno é copiar com `cat` do `OUT_DIR` e conferir byte a byte, como no plano anterior (Tarefa 4,
   Passo 1). Conferir por `grep` já falhou duas vezes.

## Tarefa 1: consultas no store (`ai-memory-store::painel`)

**Tipos:**
- `OverviewPage { path, title, kind, summary, origin_us: Option<i64>, evidence_starts_us: Vec<i64> }`,
  com os inícios das sessões de evidência do escopo, ordenados.
- `ProjectOverview { pages: Vec<OverviewPage>, sessions: Vec<(SessionId, i64 /*started*/, String /*agent*/, u32 /*pages produced*/)>,
  first_session_us, last_session_us }`.

**`ReaderPool::project_overview(ws, proj) -> StoreResult<ProjectOverview>`:**
- lê as páginas atuais (`is_latest = 1`) do escopo que não são de sistema, com a mesma regra de
  `is_system_page`. Se a regra estiver só na web, mova para o store e use nos dois lugares, sem duplicar;
- lê as sessões do escopo;
- lê as evidências de sessão dessas páginas;
- casa tudo em Rust (fato 4).

**Funções puras, testáveis sem banco:**
- `summary_line(frontmatter_json, body) -> Option<String>`;
- `iso_week_key(us) -> (i32 /*ano ISO*/, u8 /*semana*/)`, via `jiff`;
- `weekly_changes(&ProjectOverview, max_weeks: usize) -> Vec<WeekChanges>`. Cada `WeekChanges` tem
  `{ year, week, start_date, sessions, new_decisions, new_or_updated_concepts, top_session }`.
  - Uma página "nova na semana" tem `origin_us` dentro da semana.
  - Um conceito "atualizado na semana" tem algum `evidence_starts_us` na semana, com origem anterior.
  - `top_session` é a sessão com mais páginas produzidas; no empate, a de início mais antigo e, depois,
    a de menor id.
- `origin_counts_by_day(&ProjectOverview) -> BTreeMap<String /*YYYY-MM-DD UTC*/, BTreeMap<String /*kind*/, u32>>`
  conta cada página uma vez, no dia da sua origem. É usada pela Linha do tempo.

**Testes** (`tests/suite/painel.rs` e unitários das funções puras):
- a origem é o `MIN` das sessões de evidência;
- sem evidência, a origem é `None` e nunca cai em `created_at`;
- **adversarial:** uma evidência apontando para sessão de **outro projeto** e de **outro workspace** não
  conta para a origem, com controle na mesma evidência do projeto certo. Confirmar que o teste pega o
  problema: tirar o filtro de escopo das sessões, ver o teste falhar e restaurar;
- a semana ISO agrupa certo na virada de mês e na virada de ano (ex.: 2026-12-31 e 2027-01-01);
- conceito "atualizado" versus "novo";
- empate da sessão que mais produziu é determinístico;
- o resumo sai do frontmatter, cai no primeiro parágrafo e corta em 200 caracteres sem partir UTF-8.

Commit: `feat(store): project overview query with origin dates from evidence sessions`.

## Tarefa 2: telas (`ai-memory-web`)

**Rotas:**
- `/w/{ws}/{proj}` passa a ser `painel_overview::handler`;
- `/w/{ws}/{proj}/paginas` passa a ser o `project::handler` atual, sem outra mudança;
- o resto das rotas fica igual.

**Abas** (`_abas.html`): Visão geral (`aba == "visao"`, href `base_href`), Páginas (`base_href/paginas`),
Briefing, Linha do tempo e Propostas. Em `project.html`, cada pasta ganha
`id="pasta-{{ folder.name }}"`, para servir de âncora aos números da Visão geral.

**`painel_overview.rs` + `painel_overview.html`:**
- `escopo_html` e só `GET`;
- as seções da spec §2.2, na ordem: Em números, Últimas grandes mudanças (4 semanas com atividade),
  Decisões recentes (10), Conceitos centrais, Gotchas recentes (10) e Sem data de origem;
- datas em UTC, idades com `humanize_pt`;
- seção vazia mostra "Nada aqui ainda.";
- links só por `page_href` e `project_href`; nenhum `|safe`.

**Linha do tempo** (`painel_timeline.rs` e template):
- o rótulo passa de `8` para `8 sessões` (singular `1 sessão`);
- ao lado, `· 3 decisões · 5 gotchas · 1 conceito`, a partir de `origin_counts_by_day`, com o tipo no plural
  certo e sem mostrar os zeros;
- legenda acima das barras: "cada barra é um dia; o tamanho é o número de sessões".

**Testes de rota:**
- a Visão geral mostra os números e as seções com dados semeados;
- `/paginas` mostra a árvore;
- 404 para projeto inexistente e para projeto de outro workspace, com controle 200;
- `<script>` em título e resumo sai escapado;
- a Linha do tempo mostra `N sessões` e as contagens por tipo;
- a aba ativa está certa nas duas telas.

Commit: `feat(web): project overview tab and labelled timeline days`.

## Tarefa 3: gate, imagem e PR

1. Regenerar o Tailwind (fato 6) e conferir que o CSS versionado é idêntico ao gerado.
2. Gate completo, um comando por vez (fato 1).
3. `CHANGELOG.md` `[Unreleased]`:
   - `### Added`: a Visão geral;
   - `### Changed`: a página do projeto abre na Visão geral, a árvore vai para `/paginas`, e a Linha do
     tempo ganha rótulos e contagens.
4. Imagem `ai-memory-alfama:2.4.0-alfama.6`: `docker builder prune` depois do build, backup antes da troca e
   troca no compose.
5. Verificação da spec nos dados reais, os quatro itens.
6. PR no fork para `alfama/main`, com o corpo pelo template e o checklist §2.1, e a autoria conferida.
