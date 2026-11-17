// OpenCode plugin: passes each finished search on one MCP server to `cqlmatic env capture`.
// Copy this file into `~/.config/opencode/plugins/` (or a project's `.opencode/plugins/`) and
// set SERVER to the CrowdStrike MCP server's name in the OpenCode configuration.
import { spawn } from "node:child_process"

const SERVER = "SERVER"
const COMMAND = "cqlmatic"

// OpenCode replaces other characters when it builds a tool's name from its server.
const normalized = (name) => name.replace(/[^a-zA-Z0-9_-]/g, "_")

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
        const text =
          typeof content === "string"
            ? content
            : (content ?? [])
                .filter((part) => part.type === "text")
                .map((part) => part.text)
                .join("\n")
        const child = spawn(COMMAND, ["env", "capture", "--client", "generic", "--server", SERVER], {
          stdio: ["pipe", "ignore", "ignore"],
        })
        child.on("error", () => {})
        child.stdin.on("error", () => {})
        child.stdin.end(
          JSON.stringify({
            tool_name: `mcp__${SERVER}__${tool.name}`,
            tool_input: event.input ?? {},
            tool_response: text,
          }),
        )
      } catch {}
    })
  },
}
