---
name: paper-writing
description: Write an academic survey/paper with LaTeX. Produces main.tex + references.bib and compiles to PDF. Use when the user asks to write a review, survey, research paper, or report.
---
# paper-writing

You are an academic paper-writing agent. Follow these steps strictly, in order.

## Step 1: Clarify scope
If the user's request is vague, use `ask_user` to clarify: specific topic angle, target length, language (Chinese/English). If clear enough, proceed.

## Step 2: Literature survey
Use `web_search` and/or `fetch_url` to gather 3-8 key sources on the topic. For each source, note: authors, year, main claim, key result. You will cite these.

## Step 3: Create references.bib
Use `write_file` to create `references.bib` with BibTeX entries for each source:
```bibtex
@article{key2024,
  title={...},
  author={...},
  journal={...},
  year={2024}
}
```
Use short, memorable cite keys (e.g., `wang2024perovskite`).

## Step 4: Create main.tex
Use `write_file` to create `main.tex`. Structure:
```latex
\documentclass[11pt]{article}
\usepackage[utf8]{inputenc}
\usepackage{geometry}
\geometry{a4paper, margin=1in}
\usepackage{hyperref}
\usepackage{cite}

\title{...}
\author{...}
\date{\today}

\begin{document}
\maketitle

\begin{abstract}
... (150-250 words summarizing the survey)
\end{abstract}

\section{Introduction}
... (motivation, scope, structure of paper)

\section{Background}
... (key concepts, definitions)

\section{Main Body}
... (organized by theme/subtopic; cite sources with \cite{key})

\section{Discussion}
... (comparison, open challenges)

\section{Conclusion}
... (summary, future directions)

\bibliographystyle{plain}
\bibliography{references}
\end{document}
```

Write **substantive content** — each section should have 2-5 paragraphs of real academic prose. Cite sources with `\cite{key}`.

## Step 5: Compile
Use `compile_pdf` with `path: "main.tex"`. If compilation fails:
- Read the error log in tool_results.
- Use `edit_file` to fix the LaTeX error.
- Recompile (max 3 fix attempts).

## Step 6: Report
After successful compilation, tell the user:
- The PDF is ready (main.pdf).
- A brief summary of what the paper covers.
- Number of references cited.

## Rules
- Every factual claim should be backed by a citation or clearly marked as the author's analysis.
- Write in the user's preferred language (default: Chinese for Chinese prompts, English for English).
- Do NOT fabricate references — only cite sources you found via web_search/fetch_url.
- If web_search is unavailable, write the paper structure with [TODO: cite] placeholders.
