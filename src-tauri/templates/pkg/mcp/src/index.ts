import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { z } from 'zod';

const server = new McpServer({
  name: '{{slug}}',
  version: '{{version}}',
});

server.tool(
  'ping',
  'Smoke-test tool — returns "pong".',
  { message: z.string().optional() },
  async ({ message }) => ({
    content: [
      {
        type: 'text',
        text: `pong${message ? `: ${message}` : ''}`,
      },
    ],
  }),
);

const transport = new StdioServerTransport();
await server.connect(transport);
