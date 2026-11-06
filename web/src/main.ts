import { createRoot } from 'octane';
import './app.css';
import { App } from './ui/App.tsx';

// Dark when the system is.
const dark = window.matchMedia('(prefers-color-scheme: dark)');
const theme = () => document.documentElement.classList.toggle('dark', dark.matches);
theme();
dark.addEventListener('change', theme);

createRoot(document.getElementById('app')!).render(App, {});
