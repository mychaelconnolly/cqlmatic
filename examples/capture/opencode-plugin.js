// OpenCode plugin: passes each finished search on one MCP server to `cqlmatic env capture`.
// Copy this file into `~/.config/opencode/plugins/` (or a project's `.opencode/plugins/`) and
// set SERVER to the CrowdStrike MCP server's name in the OpenCode configuration.
import { spawn } from "node:child_process"

const SERVER = "SERVER"
const COMMAND = "cqlmatic"

// OpenCode replaces other characters when it builds a tool's name from its server.
const normalized = (name) => name.replace(/[^a-zA-Z0-9_-]/g, "_")

const same = (left, right) =>
  left === right ||
  (left !== null &&
    right !== null &&
    typeof left === "object" &&
    typeof right === "object" &&
    Array.isArray(left) === Array.isArray(right) &&
    Object.keys(left).length === Object.keys(right).length &&
    Object.keys(left).every((key) => same(left[key], right[key])))

// Whether the single text block parses to the structured copy.
const agrees = (content, structured) => {
  try {
    return content.length === 1 && same(JSON.parse(content[0].text), structured)
  } catch {
    return false
  }
}

export default {
  id: "cqlmatic.capture",
  async setup(ctx) {
    await ctx.tool.hook("execute.after", async (event) => {
      // Nothing here may disturb the tool call, so every failure is ignored.
      try {
        if (event.status !== "completed") return
        const tool = (await ctx.tool.list()).find((entry) => entry.id === event.tool)
        if (tool?.options?.namespace !== normalized(SERVER)) return
        const content = event.result.content
        const response = {
          content: (typeof content === "string" ? [{ type: "text", text: content }] : (content ?? []))
            .filter((part) => part.type === "text")
            .map((part) => ({ type: "text", text: part.text })),
        }
        // A structured copy that disagrees with the text goes along, so the result is refused.
        // An agreeing copy is left out: the text keeps digits that numbers here may have lost.
        const structured = event.result.output
        if (structured && typeof structured === "object" && !agrees(response.content, structured)) {
          response.structuredContent = structured
        }
        const child = spawn(COMMAND, ["env", "capture", "--client", "generic", "--server", SERVER], {
          stdio: ["pipe", "ignore", "ignore"],
        })
        child.on("error", () => {})
        child.stdin.on("error", () => {})
        child.stdin.end(
          JSON.stringify({
            tool_name: `mcp__${SERVER}__${tool.name}`,
            tool_input: event.input ?? {},
            tool_response: response,
          }),
        )
      } catch {}
    })
  },
}
