import tailwindcss from '@tailwindcss/vite';
import { bootScript } from '@uwusuite/design';
import react from '@vitejs/plugin-react';
import { defineConfig, type Plugin } from 'vite';

/**
 * The theme, contrast and motion go onto <html> before the first paint, so a
 * dark window never flashes white (@uwusuite/design's bootScript). The CSP
 * allows scripts from 'self' only, so it is a file, /boot.js, not inline.
 * UwURDP is dark until the person picks something (lib/settings.ts), where
 * the package's script would follow the system.
 */
function boot(): Plugin {
  const source = bootScript('uwurdp.settings');
  const dark = source.replace('s.theme||"system"', 's.theme||"dark"');
  if (dark === source) throw new Error('bootScript changed: the dark default no longer applies');
  return {
    name: 'uwurdp-boot',
    configureServer(server) {
      server.middlewares.use('/boot.js', (_request, response) => {
        response.setHeader('content-type', 'text/javascript');
        response.end(dark);
      });
    },
    generateBundle() {
      this.emitFile({ type: 'asset', fileName: 'boot.js', source: dark });
    },
  };
}

// Tauri drives the dev server, so the port is fixed and failures must be loud:
// silently moving to 1421 would leave the app pointing at nothing.
export default defineConfig({
  plugins: [react(), tailwindcss(), boot()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ['**/src-tauri/**', '**/target/**'],
    },
  },
  build: {
    target: 'chrome110',
    // Source maps would be packed into the release binary for nothing: the
    // source is public anyway, and dev builds have them regardless.
    sourcemap: false,
  },
});
