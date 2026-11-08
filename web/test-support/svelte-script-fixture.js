import { fileURLToPath } from 'node:url';
import { parse } from 'svelte/compiler';
import { build } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Exercise production handlers/runes without a DOM; only the test bundle gains
// inspection exports. Browser tests still validate the rendered controls.
export async function scriptFixture(componentPath, exports) {
  const entry = 'virtual:svelte-handler-fixture';
  const result = await build({
    configFile:false, root:fileURLToPath(new URL('../',import.meta.url)), logLevel:'silent',
    plugins:[{
      name:'svelte-handler-fixture',enforce:'pre',
      resolveId(id) {if(id===entry)return '\0'+id;},
      load(id) {
        if(id!=='\0'+entry)return;
        return `
          import Component from '${componentPath}';
          import { home } from '/src/maison/lib/home.svelte.js';
          import { flushSync } from 'svelte';
          import { effect_root, proxy } from 'svelte/internal/client';
          export { home, flushSync };
          export function mount(input) {
            const props=proxy(input); let component;
            const dispose=effect_root(()=>{component=Component(null,props);});
            flushSync(); return {component,dispose,props};
          }
        `;
      },
      transform(source,id) {
        if(!id.endsWith(componentPath))return;
        const {content}=parse(source,{modern:true}).instance;
        return `<script>${source.slice(content.start,content.end)}\n${exports}\n</script>`;
      },
    },svelte()],
    build:{write:false,minify:false,lib:{entry,formats:['es']},rolldownOptions:{input:entry}},
  });
  const chunk=(Array.isArray(result)?result[0]:result).output.find((entry)=>entry.type==='chunk'&&entry.isEntry);
  return import(`data:text/javascript;base64,${Buffer.from(chunk.code).toString('base64')}`);
}
