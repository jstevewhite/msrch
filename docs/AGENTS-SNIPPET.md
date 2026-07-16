# msrch — snippet for AGENTS.md / CLAUDE.md of consuming repos

Copy the block below into a repo's agent instructions once the repo is
indexed (`msrch index .`).

---

## Semantic search: msrch

This repo has a semantic index. Use `msrch` for *concept* searches ("where is
retry handling?", "what did the March report say about budget?") and `grep`
for *identifier* searches. Typical flow: msrch finds the right files, grep
pins the exact lines.

    msrch "where do we configure retries?"            # ranked hits with snippets
    msrch "budget concerns" -f filename               # paths only (like grep -l)
    grep -n "max_retries" $(msrch "retry config" -f filename --limit 3)

Filters (query only):

    msrch "quarterly numbers" --path 2026/07          # path substring
    msrch "action items" --after 7d                   # modified in the last 7 days
    msrch "planning" --after 2026-07-01 --before 2026-08-01
    msrch "config parsing" -m 0.7                     # per-query similarity floor

Notes:
- `--format json` gives structured output (file_path, chunk_index, similarity,
  context, content).
- Indexed content includes extracted text from HTML, PDF (text layer), and
  .docx files — searchable even though grep can't read them.
- If this repo's `.msrch/config.toml` sets `query.auto_index = true`, results
  are always fresh; otherwise run `msrch index .` after big changes.
- `--rerank` trades speed for precision when the top hits look off.
