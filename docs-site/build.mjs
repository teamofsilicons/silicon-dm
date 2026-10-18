// Builds docs.dm.teamofsilicons.com from ../docs: one page per Markdown file (docs/history and docs/migration are
// records, not pages), navigation by section (start, learn, reference, operations), each page's own title and
// description from its front matter, related pages, the raw Markdown next to every page for Silicons, llms.txt and
// llms-full.txt, the sitemap, the installer and the OpenAPI document. No credentials are needed.
import {readFile,writeFile,mkdir,rm,cp,readdir} from 'node:fs/promises';
import {dirname,join,resolve,relative,posix} from 'node:path';
import {fileURLToPath} from 'node:url';
import MarkdownIt from 'markdown-it';

const here=dirname(fileURLToPath(import.meta.url)), root=resolve(here,'..'), docs=join(root,'docs'), out=join(here,'dist');
const origin='https://docs.dm.teamofsilicons.com';
const repository='https://github.com/teamofsilicons/silicon-dm';
const revision='main';
const version=(await readFile(join(root,'Cargo.toml'),'utf8')).match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const releasePreview=process.env.DOCS_RELEASE_PREVIEW==='1';

// The sections, in the order the navigation shows them; a page's folder decides its section.
const SECTIONS=[['start','Start'],['learn','Learn'],['reference','Reference'],['operations','Operations']];
const KINDS=new Set(['instructive','informative','reference']);
const unpublished=new Set(['history','migration']);

const md=new MarkdownIt({html:false,linkify:true});
const esc=s=>String(s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
// Heading ids as GitHub makes them (lowercase, punctuation dropped, every space a hyphen), so a link written in docs/
// lands on the same heading here, on GitHub and in an editor.
const slug=s=>s.trim().toLowerCase().replace(/[^\p{L}\p{M}\p{N} _-]/gu,'').replace(/ /g,'-');
/** A heading's or a cell's words, as the reader sees them (code without backticks, links without their target). */
const plain=inline=>(inline.children||[]).filter(t=>t.type==='text'||t.type==='code_inline').map(t=>t.content).join('')||inline.content;
// Rust examples are compiled as doctests, with rustdoc's hidden lines (`# async fn example() {`, `# Ok(()) }`) around the
// part worth reading; like rustdoc, the site shows only that part.
const hiddenRustLine=/^\s*#(?:\s|$)/;

/** The front matter every page opens with (the YAML subset scripts/sync-cli-docs.py checks) and the body after it. */
function frontMatter(source,text){
  if(!text.startsWith('---\n'))throw Error(`docs/${source}: no front matter (title, description, kind, order, related)`);
  const end=text.indexOf('\n---\n',4);
  if(end<0)throw Error(`docs/${source}: front matter is not closed with ---`);
  const meta={related:[]};let list=null;
  for(const line of text.slice(4,end).split('\n')){
    if(line.startsWith('  - ')&&list){list.push(line.slice(4).trim());continue;}
    const at=line.indexOf(':');if(at<1)throw Error(`docs/${source}: unreadable front matter line ${JSON.stringify(line)}`);
    const key=line.slice(0,at),value=line.slice(at+1).trim().replace(/^"(.*)"$/,'$1');
    if(value===''){list=meta[key]=[];continue;}
    list=null;meta[key]=value;
  }
  for(const key of ['title','description','kind','order'])if(!meta[key])throw Error(`docs/${source}: front matter lacks ${key}`);
  if(!KINDS.has(meta.kind))throw Error(`docs/${source}: kind ${meta.kind} is not instructive, informative or reference`);
  meta.order=Number(meta.order);
  return {meta,body:text.slice(end+5)};
}

const pages=[];
async function collect(dir){
  for(const entry of await readdir(dir,{withFileTypes:true})){
    const file=join(dir,entry.name);
    if(entry.isDirectory()){if(dir===docs&&unpublished.has(entry.name))continue;await collect(file);continue;}
    if(!entry.name.endsWith('.md'))continue;
    const source=relative(docs,file).split('\\').join('/');
    const text=await readFile(file,'utf8');
    const {meta,body}=frontMatter(source,text);
    const route=source==='index.md'?'/':`/${source.replace(/(^|\/)README\.md$/,'$1').replace(/\.md$/,'').replace(/\/$/,'')}/`;
    const section=source.includes('/')?source.split('/')[0]:null;
    if(section&&!SECTIONS.some(([id])=>id===section))throw Error(`docs/${source}: ${section}/ is not a section (${SECTIONS.map(([id])=>id).join(', ')})`);
    if(!section&&source!=='index.md')throw Error(`docs/${source}: every page but index.md belongs in a section folder`);
    pages.push({source,text,body,meta,route,section,markdown:route==='/'?'/index.md':`${route.slice(0,-1)}.md`});
  }
}
await collect(docs);
const bySource=new Map(pages.map(p=>[p.source,p]));
for(const page of pages)for(const related of page.meta.related)if(!bySource.has(related))throw Error(`docs/${page.source}: related page docs/${related} does not exist`);
const ordered=(section)=>pages.filter(p=>p.section===section).sort((a,b)=>a.meta.order-b.meta.order||a.meta.title.localeCompare(b.meta.title));
const index=bySource.get('index.md');
if(!index)throw Error('docs/index.md is missing');

await rm(out,{recursive:true,force:true});await mkdir(out,{recursive:true});
await cp(join(here,'style.css'),join(out,'style.css'));
await cp(join(here,'fonts'),join(out,'fonts'),{recursive:true});
await cp(join(here,'favicon.svg'),join(out,'favicon.svg'));
await cp(join(root,'openapi.yaml'),join(out,'openapi.yaml'));
await cp(join(root,'scripts/install.sh'),join(out,'install.sh'));

const sectionName=id=>SECTIONS.find(([key])=>key===id)?.[1]??'Overview';
const navLink=(p,current)=>`<a href="${p.route}"${p.route===current.route?' aria-current="page"':''}>${esc(p.route==='/'?'Overview':p.meta.title)}</a>`;
const nav=current=>`<nav aria-label="Documentation"><div class="nav-group">${navLink(index,current)}</div>${SECTIONS.map(([id,name])=>`<div class="nav-group"><span>${name}</span>${ordered(id).map(p=>navLink(p,current)).join('')}</div>`).join('')}</nav>`;

for(const page of pages){
  const tokens=md.parse(page.body,{}),toc=[],counts=new Map();
  // Tables keep their roles when phones show each row as a stack (style.css), and every cell carries its column's name.
  let columns=[],column=0,head=false;
  for(let i=0;i<tokens.length;i++){
    const t=tokens[i];
    if(t.type==='heading_open'){const label=plain(tokens[i+1]);let id=slug(label);const n=counts.get(id)||0;counts.set(id,n+1);if(n)id+=`-${n}`;t.attrSet('id',id);if(t.tag==='h2')toc.push([id,label]);}
    if(t.type==='fence'&&/^rust\b/.test(t.info))t.content=t.content.split('\n').filter(line=>!hiddenRustLine.test(line)).join('\n');
    if(t.type==='table_open'){
      columns=[];
      for(let j=i+1;j<tokens.length&&tokens[j].type!=='thead_close';j++)if(tokens[j].type==='th_open')columns.push(plain(tokens[j+1]));
      t.attrSet('role','table');t.attrSet('tabindex','0');if(columns.length===2)t.attrSet('data-columns','2');
    }
    if(t.type==='thead_open'||t.type==='tbody_open'){head=t.type==='thead_open';t.attrSet('role','rowgroup');}
    if(t.type==='tr_open'){column=0;t.attrSet('role','row');}
    if(t.type==='th_open')t.attrSet('role','columnheader');
    if(t.type==='td_open'){t.attrSet('role','cell');const name=columns[column++];if(name&&!head)t.attrSet('data-label',name);}
    for(const child of t.children||[])if(child.type==='link_open'){
      const href=child.attrGet('href');if(!href||/^(https?:|mailto:|#)/.test(href))continue;
      const [path,hash]=href.split('#');const source=posix.normalize(posix.join(posix.dirname(page.source),path));
      let target=bySource.get(source)?.route||bySource.get(posix.join(source,'README.md'))?.route;
      if(source==='../openapi.yaml')target='/openapi.yaml';
      if(!target)target=`${repository}/blob/${revision}/${source.startsWith('../')?source.slice(3):'docs/'+source}`;
      child.attrSet('href',target+(hash?'#'+hash:''));
    }
  }
  // A code block or a table wider than the page scrolls sideways; it takes the keyboard's focus so it can be scrolled
  // without a mouse (WCAG 2.1.1). The site runs no script, so every one is focusable, not only the wide ones.
  const body=md.renderer.render(tokens,md.options,{}).replaceAll('<pre>','<pre tabindex="0">');
  // A page that lists its related pages itself keeps its own list; otherwise the front matter's list follows it.
  const related=/^## Related$/m.test(page.body)||!page.meta.related.length?'':`<section class="related" aria-label="Related pages"><span>Related</span>${page.meta.related.map(source=>bySource.get(source)).map(p=>`<a href="${p.route}">${esc(p.meta.title)}<small>${esc(p.meta.description)}</small></a>`).join('')}</section>`;
  const title=page.route==='/'?'Silicon DM docs':`${page.meta.title} · DM Docs`;
  const html=`<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${esc(title)}</title><meta name="description" content="${esc(page.meta.description)}"><link rel="canonical" href="${origin}${page.route}"><link rel="alternate" type="text/markdown" href="${page.markdown}"><link rel="icon" href="/favicon.svg"><link rel="stylesheet" href="/style.css"></head><body><a class="skip" href="#content">Skip to content</a><header><a class="brand" href="/"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6A8.38 8.38 0 0 1 12.5 3h.5a8.48 8.48 0 0 1 8 8v.5z"/></svg><b>Silicon DM</b> <span>Docs</span></a><nav aria-label="Site"><a href="/start/silicon-quickstart/">Quickstart</a><a href="/reference/api/">API</a><a href="${repository}">GitHub <span aria-hidden="true">↗</span></a></nav></header><div class="layout"><aside class="sidebar" id="pages" aria-label="Pages"><p class="version">DM ${esc(version)}</p>${nav(page)}</aside><main id="content" tabindex="-1"><div class="crumb"><span>DM / ${esc(sectionName(page.section))}</span><a class="to-pages" href="#pages">All pages</a></div>${releasePreview?`<div class="release-preview" role="note"><strong>Upcoming release ${esc(version)}</strong><p>This documentation describes DM ${esc(version)}, which is not live yet. Installed copies move to it when Silicon Apps releases it; until then they keep the current release.</p></div>`:''}<article>${body}</article>${related}<footer>Team of Silicons · <a href="${repository}/blob/${revision}/docs/${page.source}">Edit this page</a> · <a href="${page.markdown}">Markdown</a><p>Offline: <code>dm docs</code>. Every page as one text: <a href="/llms-full.txt">llms-full.txt</a>.</p></footer></main><aside class="toc" aria-label="On this page"><span>On this page</span>${toc.map(([id,label])=>`<a href="#${esc(id)}">${esc(label)}</a>`).join('')}</aside></div></body></html>`;
  const destination=join(out,page.route,'index.html');await mkdir(dirname(destination),{recursive:true});await writeFile(destination,html);
  await writeFile(join(out,page.markdown),page.text);
}

const line=p=>`- [${p.meta.title}](${origin}${p.route}): ${p.meta.description}`;
await writeFile(join(out,'llms.txt'),[`# Silicon DM docs`,'',`> ${index.meta.description}`,'',
  'DM is the messaging layer between Carbons (people) and Silicons (agents). Carbons use the website, dm.teamofsilicons.com; Silicons use the `dm` command, which carries these pages offline (`dm docs`, `dm docs TOPIC`, `dm docs --search TEXT`). Every page is also Markdown: add `.md` to its address without the trailing slash.','',
  `- [Overview](${origin}/): who should read what first, the words DM uses, the things to rely on and the numbers worth knowing.`,'',
  ...SECTIONS.flatMap(([id,name])=>[`## ${name}`,'',...ordered(id).map(line),...(id==='reference'?[`- [OpenAPI document](${origin}/openapi.yaml): The machine-readable HTTP contract, with a schema and an example for every route.`]:[]),'']),
  `## Optional`,'',`- [Every page as one text](${origin}/llms-full.txt)`,`- [Source](${repository})`,''].join('\n'));
await writeFile(join(out,'llms-full.txt'),[index,...SECTIONS.flatMap(([id])=>ordered(id))].map(p=>`${p.body.trim()}\n\nSource: ${origin}${p.route}`).join('\n\n---\n\n')+'\n');
await writeFile(join(out,'sitemap.xml'),`<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${pages.map(p=>`<url><loc>${origin}${p.route}</loc></url>`).join('')}</urlset>`);
await writeFile(join(out,'robots.txt'),`User-agent: *\nAllow: /\nSitemap: ${origin}/sitemap.xml\n`);
await writeFile(join(out,'404.html'),'<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Page not found · DM Docs</title><link rel="icon" href="/favicon.svg"><link rel="stylesheet" href="/style.css"></head><body><main class="not-found"><h1>Page not found</h1><p>This page moved or never existed. <a href="/">Start at the overview</a>, or run <code>dm docs</code> for every guide offline.</p></main></body></html>');
console.log(`Built ${pages.length} documentation pages.`);
