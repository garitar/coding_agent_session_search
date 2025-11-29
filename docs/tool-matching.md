# Tool Matching in cass

This document explains how `cass search --tools` matches and displays tool calls alongside search results, including known gotchas and edge cases.

## Overview

When you use `--tools` with search, cass extracts tool calls from the source JSONL file around the matched message. This provides context about what tools were used during the conversation turn.

## How Tool Matching Works

### Search Window Direction

The search direction for tools depends on the role of the matched message:

| Matched Role | Direction | Window Size | Rationale |
|--------------|-----------|-------------|-----------|
| `user` | Forward | 30 lines | User message triggers assistant response with tools |
| `assistant` | Backward | 20 lines | Assistant message follows the tools it called |

This asymmetry exists because:
- When a user message matches, the interesting tools are in the **response** (forward)
- When an assistant message matches, the tools were called **before** that text (backward)

### Turn Boundary Detection

Tool extraction stops at conversation turn boundaries to avoid showing tools from different exchanges. A turn boundary is detected when:

1. A new user text message appears (not a `tool_result`)
2. An assistant message with text content appears (not just `tool_use`)

This prevents showing tools from unrelated turns that happen to be nearby in the file.

## Gotchas and Edge Cases

### 1. Line Number Offset Alignment

**Problem**: JSONL line numbers are 1-indexed, but array indices are 0-indexed. The search index stores line numbers for hit positions, which must be correctly translated when scanning the source file.

**Solution**: When iterating over file lines, we use `enumerate()` which gives 0-indexed positions, then add 1 to get the 1-indexed line number for comparison with the search hit's `line_number` field.

### 2. Rejected Tool Calls

**Problem**: Claude may attempt multiple tool calls before one succeeds. Rejected or failed tool attempts appear in the JSONL before successful calls.

**Example scenario**:
```
Line 10: [assistant] tool_use: Read /path/that/fails (rejected by permission)
Line 11: [user] tool_result: error - permission denied
Line 12: [assistant] tool_use: Read /different/path (success)
Line 13: [user] tool_result: file contents...
```

When the search matches the final assistant response (line 14+), the backward search may capture both the failed and successful tool calls. This is intentional - it shows the full context of the agent's attempts.

### 3. Window Size Limits

**Problem**: Without limits, tool extraction could capture hundreds of lines in long tool-heavy sessions.

**Current limits**:
- Forward (user matches): 30 lines
- Backward (assistant matches): 20 lines

These limits are heuristics that work well for typical conversations. Very long tool outputs may be truncated.

### 4. Tool Input/Output Truncation

**Problem**: Tool inputs and outputs can be very large (entire files, API responses).

**Solution**: The `--tools` flag accepts an optional character limit:
- `--tools` - Default 200 character limit
- `--tools 0` - No limit (show full content)
- `--tools 500` - Custom 500 character limit

Truncated content ends with `...`.

### 5. Claude Code vs Codex Format Differences

The JSONL format differs between connectors:

**Claude Code format**:
```json
{"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "toolu_xxx", "name": "Read", "input": {...}}]}}
{"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_xxx", "content": "..."}]}}
```

**Codex format**:
```json
{"type": "response_item", "payload": {"type": "function_call", "name": "Read", "arguments": "...", "call_id": "call_xxx"}}
{"type": "response_item", "payload": {"type": "function_call_output", "call_id": "call_xxx", "output": "..."}}
```

The tool extraction handles both formats, matching tool calls with their results via ID.

## Debugging Tips

1. **Missing tools**: Check if the matched message role is correct. User matches look forward, assistant matches look backward.

2. **Too many tools**: The window might span multiple turns. Check for missing turn boundary detection.

3. **Wrong tools**: The line number offset might be misaligned. Verify the search hit's `line_number` matches the actual source file.

4. **No tool results**: Some tools may not have results yet (streaming) or the result may be beyond the search window.

## Implementation Reference

The core tool extraction logic is in `fetch_tools_from_source()` in `src/lib.rs`. Key components:

- Window calculation based on role
- Turn boundary detection
- Tool use/result matching via ID
- Input/output truncation
