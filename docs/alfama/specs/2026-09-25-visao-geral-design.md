# A página do projeto lista 357 arquivos, mas não diz o que o projeto decidiu, aprendeu e mudou

> Spec do fork `maxeinstein-dev/ai-memory`, continuação de
> [`2026-09-24-painel-web-alfama-design.md`](2026-09-24-painel-web-alfama-design.md): as mesmas regras
> de segurança (§2.1) e o mesmo fluxo de trabalho (§2.2) valem aqui. Pedido do usuário em 2026-09-25,
> depois de usar as telas nos dados reais.

## 1. O problema, medido (2026-09-25, projeto `aw-senai-sgw`)

**1.1. Na Linha do tempo, o número ao lado de cada data não diz o que é.** A tela mostra
`2026-09-17  8`. O 8 é o número de sessões daquele dia, mas não há rótulo nem legenda. A tela também não
diz, por dia, o que as sessões deixaram de conhecimento. Esse dado existe (`page_evidence`), mas aparece
só dentro de cada sessão, na lista abaixo das barras.

**1.2. A página do projeto é uma árvore de arquivos.** São 88 decisões, 56 conceitos, 109 gotchas,
3 regras e 98 resumos de sessão, listados por pasta e em ordem alfabética. A árvore não mostra:

- quantas páginas há de cada tipo;
- o que foi decidido ou aprendido **por último**;
- quais conceitos são centrais (os que o briefing entrega ao agente);
- o que mudou no projeto nas últimas semanas.

**1.3. A data da página não é a data do conhecimento.** As 357 páginas atuais têm `created_at` em
2026-09-24 (354) ou 2026-09-25 (3), os dias em que o LLM gerou as páginas a partir do histórico. Ordenar
por `created_at` ou `updated_at` põe tudo no mesmo dia. A data real é a da **sessão de onde a página
veio**, registrada em `page_evidence` (`source_kind = 'session'`). Essas sessões já estão com as datas
reparadas (PR 5 do fork). Cobertura medida:

| Tipo | Páginas | Com sessão de origem | % |
|---|---|---|---|
| decisões | 88 | 75 | 85% |
| gotchas | 109 | 100 | 92% |
| conceitos | 56 | 29 | 52% |

## 2. O desenho

A mesma divergência mínima do painel: arquivos novos (`routes/painel_visao_geral.rs`,
`templates/painel_visao_geral.html`, consultas em `ai-memory-store::painel`), só `GET`, só leitura,
sem LLM e sem dependência nova. O escopo é resolvido por `escopo_html`.

### 2.1. Data de uma página = início da sessão de origem mais antiga

`data_de_origem(página) = MIN(sessions.started_at)` sobre as evidências `source_kind = 'session'` da
versão atual, com a sessão **no mesmo workspace e projeto** da página. Sem evidência de sessão, a página
fica **sem data**. Ela aparece em "sem data de origem" e nunca é ordenada por `created_at`, que é a data
da geração (§1.3). A mesma regra vale para as duas telas abaixo.

### 2.2. Nova aba **Visão geral**, a entrada do projeto

`/w/{workspace}/{project}` passa a abrir a Visão geral. A árvore atual continua inteira na aba
**Páginas** (`/w/{workspace}/{project}/paginas`), e os links antigos continuam funcionando (§2.4).
Seções, de cima para baixo:

1. **Em números:** decisões, conceitos, gotchas, regras, procedimentos e sessões. Cada número tem
   rótulo e leva à pasta correspondente na aba Páginas. Mostra também o período coberto: da primeira à
   última sessão.
2. **Últimas grandes mudanças:** resumo por semana (ISO, segunda a domingo), das 4 semanas mais
   recentes com atividade. Cada semana traz:
   - o número de sessões;
   - as decisões novas (título e link), em ordem de data de origem;
   - os conceitos novos ou atualizados;
   - a sessão que mais produziu páginas na semana, com o agente e o número de páginas.

   "Novo na semana" = data de origem (§2.1) dentro da semana. Montado só dos dados, sem LLM (decisão do
   usuário em 2026-09-25).
3. **Decisões recentes:** as 10 mais recentes por data de origem, com o resumo de uma linha
   (`frontmatter.summary`; sem ele, o primeiro parágrafo do corpo, cortado em 200 caracteres) e a data.
4. **Conceitos centrais:** os conceitos que o briefing considera centrais, na mesma ordem e com o mesmo
   limite (`BRIEF_CORE_PAGES_LIMIT`), pela mesma função do briefing, sem regra paralela. Se o briefing
   não separar conceitos de outros tipos, a seção mostra os centrais de qualquer tipo, com o tipo ao
   lado, e o título diz isso.
5. **Gotchas recentes:** os 10 mais recentes por data de origem, com o resumo de uma linha.
6. **Sem data de origem:** quantas páginas de cada tipo não têm sessão de origem, com link para a lista
   (aba Páginas). A lacuna fica visível em vez de escondida.

Cada seção vazia mostra "Nada aqui ainda.".

### 2.3. Linha do tempo com rótulos e o que cada dia produziu

- Cada barra ganha o rótulo por extenso: `8 sessões` (singular `1 sessão`), além de uma legenda curta
  acima das barras: "cada barra é um dia; o tamanho é o número de sessões".
- Ao lado do rótulo, o que o dia produziu, contado por tipo: `8 sessões · 3 decisões · 5 gotchas ·
  1 conceito`. Conta a página no dia da **sua** data de origem (§2.1), para cada página aparecer uma vez
  só, no dia em que nasceu, e não em todo dia em que foi citada.
- A lista de sessões por dia continua como está.

### 2.4. Rotas

| Rota | Conteúdo |
|---|---|
| `/w/{ws}/{proj}` | Visão geral (nova) |
| `/w/{ws}/{proj}/paginas` | árvore atual de páginas (conteúdo da antiga `/w/{ws}/{proj}`) |
| `/w/{ws}/{proj}/p/{path}` | página, sem mudança |
| demais abas | sem mudança |

As abas ficam: **Visão geral** · Páginas · Briefing · Linha do tempo · Propostas.

## 3. Segurança (herdada, §2.1 da spec do painel)

- Só `GET`, dentro do router protegido; nenhuma escrita; nada fora do `WriterHandle`.
- Escopo por `escopo_html`: projeto inexistente ou de outro workspace → 404.
- As consultas novas filtram página, evidência **e sessão** por `(workspace_id, project_id)`. Uma
  evidência que aponte para sessão de outro projeto não conta para a data nem para a semana.
- Título, resumo e caminho passam pelo escape do askama; nenhum `|safe`. O resumo de uma linha é texto
  puro, sem render de Markdown.
- Páginas são compartilhadas no projeto (invariante 16): nenhum filtro por dono.

## 4. Testes

- **Store:** a data de origem é o `MIN` das sessões de evidência; sem evidência, a página fica sem data
  (e não recebe `created_at`); uma evidência de sessão de **outro projeto** e de **outro workspace** não
  conta (adversarial, com controle; o teste morde sem o filtro); a semana ISO agrupa certo na virada de
  mês e de ano; a sessão que mais produziu é determinística no empate.
- **Rota:** `/w/{ws}/{proj}` mostra a Visão geral com os números certos; `/paginas` mostra a árvore;
  404 para projeto inexistente e de outro workspace (controle 200); `<script>` em título e resumo sai
  escapado; a Linha do tempo mostra `N sessões` e as contagens por tipo.
- **CSS:** `static/tailwind.css` regenerado e conferido byte a byte (o `grep` já falhou duas vezes).

## 5. Entrega

Um PR no fork, `alfama/feat-visao-geral`: spec, consultas, as duas telas, CHANGELOG em `### Added`
(Visão geral) e `### Changed` (Linha do tempo com rótulos), imagem `2.4.0-alfama.6` e troca no servidor
com backup.

## Verificação (nos dados reais, depois da troca)

1. A página do `aw-senai-sgw` abre na Visão geral, com 88 decisões, 56 conceitos, 109 gotchas, 3 regras
   e 98 sessões.
2. "Decisões recentes" traz datas entre 2026-08-04 e 2026-09-24, não 2026-09-24 para todas.
3. "Sem data de origem" mostra 13 decisões, 27 conceitos e 9 gotchas.
4. Na Linha do tempo, `2026-09-17` mostra `8 sessões` e as contagens do que o dia produziu.

## O que esta spec NÃO entrega

- Texto escrito por LLM (resumo semanal em prosa), fora por decisão do usuário.
- Datar as páginas sem sessão de origem (vieram do bootstrap sobre o repositório, não de uma sessão).
- Mudança no conteúdo das páginas ou na consolidação.

## O que esta spec deliberadamente NÃO decide

- Se "semana" deveria virar "mês" nos projetos com pouca atividade. Começa com semana e 4 semanas com
  atividade; ajusta depois de usar.
