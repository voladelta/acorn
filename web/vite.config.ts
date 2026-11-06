// The window, built into web/dist, which acorn-server serves. `npm run dev`
// serves it with hot reload and sends /api on to the server, by default
// on 127.0.0.1:8484 ($ACORN_ADDR).
import { octane } from '@octanejs/vite-plugin';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

const server = `http://${process.env.ACORN_ADDR ?? '127.0.0.1:8484'}`;

export default defineConfig({
  plugins: [octane(), tailwindcss()],
  base: './',
  build: { outDir: 'dist', emptyOutDir: true },
  server: { proxy: { '/api': server } },
});
