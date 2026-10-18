// Checks the built site: canonical URLs, a description on every page, every local link and its #anchor (on the same page
// or another one), every page in the navigation and in llms.txt, and the Markdown copy of every page.
import {readFile,readdir,stat} from 'node:fs/promises';
import {join,resolve,dirname,relative} from 'node:path';
import {fileURLToPath} from 'node:url';
const root=join(dirname(fileURLToPath(import.meta.url)),'dist'),pages=[];
async function walk(dir){for(const e of await readdir(dir,{withFileTypes:true})){const p=join(dir,e.name);if(e.isDirectory())await walk(p);else if(e.name==='index.html')pages.push(p);}}
await walk(root);const failures=[];
const routeOf=page=>'/'+relative(root,dirname(page)).split('\\').join('/')+(dirname(page)===root?'':'/');
const llms=await readFile(join(root,'llms.txt'),'utf8');
const html=new Map();
const read=async file=>{if(!html.has(file))html.set(file,await readFile(file,'utf8').catch(()=>null));return html.get(file);};
for(const page of pages){
 const text=await read(page),route=routeOf(page).replace('//','/');
 if(!text.includes(`rel="canonical" href="https://docs.dm.teamofsilicons.com${route}"`))failures.push(`${page}: canonical`);
 if(!/<meta name="description" content="[^"]{20,}">/.test(text))failures.push(`${page}: description`);
 if(!llms.includes(`https://docs.dm.teamofsilicons.com${route})`))failures.push(`${page}: not in llms.txt`);
 for(const [,href,id] of text.matchAll(/href="(\/[^"#]*)(?:#([^"]*))?"/g)){
  const target=resolve(root,'.'+decodeURI(href));if(!target.startsWith(root))throw Error('Escaping link');
  const file=href.endsWith('/')?join(target,'index.html'):target;
  try{await stat(file)}catch{failures.push(`${page}: ${href}`);continue}
  // A link to a heading of another page must find that heading there.
  if(id&&file.endsWith('.html')&&!(await read(file)).includes(`id="${id}"`))failures.push(`${page}: ${href}#${id} (no such heading)`);
 }
 for(const [,id] of text.matchAll(/href="#([^"]+)"/g))if(!text.includes(`id="${id}"`))failures.push(`${page}: #${id}`);
 // rustdoc's hidden lines stay out of the examples (build.mjs).
 for(const [,code] of text.matchAll(/<code class="language-rust[^"]*">([\s\S]*?)<\/code>/g))if(/^\s*#(?:\s|$)/m.test(code))failures.push(`${page}: a hidden rustdoc line shows in an example`);
}
const home=await read(join(root,'index.html'));
for(const page of pages){const route=routeOf(page).replace('//','/');if(!home.includes(`href="${route}"`))failures.push(`${route}: missing from the navigation`);}
if(failures.length)throw Error(failures.join('\n'));
console.log(`Verified ${pages.length} pages: canonical URLs, descriptions, local links and anchors (across pages too), navigation, llms.txt, Markdown copies and examples.`);
