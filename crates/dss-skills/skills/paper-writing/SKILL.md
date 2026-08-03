---
name: paper-writing
description: Write an academic survey/paper. Use when the user asks to write a review, survey, or research paper. Produces main.tex and references.bib and compiles to PDF.
---
# paper-writing

Orchestrates writing an academic paper:
1. Clarify the research question with the user (use ask_user).
2. Survey literature (web_search / search_papers).
3. Outline sections (introduction, background, methods, results, discussion, conclusion).
4. Write main.tex section by section (write_file), with references.bib.
5. Compile to PDF (compile_pdf) and fix errors iteratively.

Keep claims grounded in retrieved sources. Cite via \cite{key} matching references.bib entries.
