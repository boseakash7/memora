import { mount } from 'svelte'
import './app.css'
import './motion.css'
import App from './App.svelte'
import { initMotion } from './lib/motion.svelte'

// Before the first render, so the first page already animates at the right level.
initMotion()

const app = mount(App, {
  target: document.getElementById('app')!,
})

export default app
