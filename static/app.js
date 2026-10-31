'use strict';
const $ = id => document.getElementById(id);
const number = n => new Intl.NumberFormat('en-US').format(n);
const compact = n => new Intl.NumberFormat('en-US', {notation:'compact',maximumFractionDigits:1}).format(n);
const date = s => new Date(s).toLocaleString('en-GB', {timeZone:'UTC',dateStyle:'medium',timeStyle:'short'}) + ' UTC';
function text(id, value) { $(id).textContent = value; }
function el(tag, cls, content) { const n=document.createElement(tag); if(cls)n.className=cls; if(content!==undefined)n.textContent=content; return n; }
function svg(tag,attrs={}) { const n=document.createElementNS('http://www.w3.org/2000/svg',tag); for(const[k,v]of Object.entries(attrs))n.setAttribute(k,v); return n; }
function placeholder(container,message) { container.replaceChildren(el('div','placeholder',message)); }
function chart(rows) {
  const box=$('chart');box.replaceChildren();$('daily-values').replaceChildren();
  if(!rows.length){placeholder(box,'No reported usage in this window');text('chart-range','');return;}
  const w=640,h=220,left=44,base=188,top=12;
  const first=Date.parse(rows[0].date),last=Date.parse(rows.at(-1).date),span=Math.round((last-first)/86400000)+1;
  const max=Math.max(...rows.map(d=>d.usage.total),1), width=(w-left-4)/span;
  const root=svg('svg',{viewBox:`0 0 ${w} ${h}`,role:'img','aria-label':`Daily token usage across ${span} UTC days. Exact values in the table below.`});
  for(let i=0;i<=3;i++){const y=base-(base-top)*i/3;root.append(svg('line',{x1:left,y1:y,x2:w,y2:y,class:'gridline'}));const label=svg('text',{x:left-8,y:y+3,'text-anchor':'end'});label.textContent=compact(max*i/3);root.append(label);}
  for(const row of rows){
    const x=left+Math.round((Date.parse(row.date)-first)/86400000)*width;
    const bar=svg('rect',{x:x+width*.14,y:base-row.usage.total/max*(base-top),width:Math.max(.5,width*.72),height:Math.max(1,row.usage.total/max*(base-top)),rx:Math.min(3,width*.1),class:'bar'});
    const title=svg('title');title.textContent=`${row.date}: ${number(row.usage.total)} tokens`;bar.append(title);root.append(bar);
    const tr=el('tr');for(const v of[row.date,number(row.usage.input),number(row.usage.output),number(row.usage.total)])tr.append(el('td','',v));$('daily-values').append(tr);
  }
  for(const [value,x,anchor]of [[rows[0].date,left,'start'],[rows.at(-1).date,w,'end']]){const label=svg('text',{x,y:212,'text-anchor':anchor});label.textContent=value;root.append(label);}
  box.append(root);text('chart-range',`${rows.length} days with records`);
}
function projects(rows) {
  const box=$('projects');box.replaceChildren();text('project-count',`${rows.length} total`);
  if(!rows.length){placeholder(box,'Project usage will appear here');return;}
  for(const row of rows.slice(0,5)){
    const item=el('div','project'),heading=el('div','project-heading');const name=el('span','project-name',row.name.split('/').filter(Boolean).at(-1)||row.name);name.title=row.name;
    heading.append(name,el('strong','',compact(row.usage.total)));
    const track=el('div','track'),bar=svg('svg',{viewBox:'0 0 100 6',preserveAspectRatio:'none','aria-hidden':'true'});bar.append(svg('rect',{width:Math.max(1,row.usage.total/Math.max(rows[0].usage.total,1)*100),height:6,rx:3}));track.append(bar);item.append(heading,track);box.append(item);
  }
}
function render(data) {
  $('demo').hidden=!data.demo;
  text('total',data.totals?compact(data.totals.total):'—');$('total').title=data.totals?`${number(data.totals.total)} tokens`:'';
  text('total-note',data.totals?`${number(data.totals.records)} measured usage records`:'No measured records in this window');
  text('today',data.today?compact(data.today.total):'—');text('today-note',data.today?`${number(data.today.total)} tokens processed`:'No reported records today');
  text('sessions',data.totals?number(data.sessions):'—');
  text('updated',`Usage snapshot: ${date(data.generated_at)}`);
  $('empty').hidden=data.status==='measured_local_only';
  text('empty',data.status==='unavailable'?'No measurable local usage is available. Run local Codex, select its home with --codex-home, or import a Token Grove export. Use --demo to explore a separate, labeled example.':'No usage records in this time window. Try “All retained records”. This does not establish zero usage.');
  chart(data.daily);projects(data.projects);
  const parts=$('composition');parts.replaceChildren();const t=data.totals;
  for(const [name,value]of [['Input · includes cache',t?number(t.input):'—'],['Cached input · subset',t&&t.cache_known?number(t.cached):'Unknown'],['Output · includes reasoning',t?number(t.output):'—']]){const row=el('div','composition-row');row.append(el('span','',name),el('strong','',value));parts.append(row);}
  const c=data.coverage;const gaps=c.unreadable+c.malformed+c.incomplete_lines+c.conflicts+c.skipped_usage+c.legacy_suppressed+c.counter_resets+c.unknown_baselines;
  text('coverage-status',data.reload?.error?'Last valid snapshot':data.demo?'Demo':data.status==='unavailable'?'Unavailable':gaps?'Partial coverage':'Local records only');
  text('source',data.source);text('record-span',data.first_record?`Retained record span: ${date(data.first_record)} → ${date(data.last_record)}`:'No valid record timestamps available.');
  const stats=$('coverage-stats');stats.replaceChildren();for(const[label,n]of [['files read',c.files_read],['duplicates ignored',c.duplicates],['inherited records excluded',c.excluded_history]]){const item=el('span');item.append(el('b','',number(n)),document.createTextNode(' '+label));stats.append(item);}
  const warnings=$('warnings');warnings.replaceChildren();
  const messages=[...c.notes];
  if(data.reload?.error)messages.unshift(`Snapshot update failed; showing last valid data. ${data.reload.error}`);
  if(data.reload)messages.push(`Automatic reload checked ${date(data.reload.last_checked)}. ${number(data.reload.reloads)} validated updates since startup.`);
  for(const [label,n]of [['Unreadable files/directories',c.unreadable],['Malformed or oversized log lines',c.malformed],['Incomplete trailing lines (retried next scan)',c.incomplete_lines],['Conflicting record IDs excluded',c.conflicts],['Usage events skipped (missing, invalid, or unsupported)',c.skipped_usage],['Legacy events excluded in sessions with response records',c.legacy_suppressed],['Counter resets; only reported last usage counted',c.counter_resets],['Unknown cumulative baselines; no historical total guessed',c.unknown_baselines]])if(n)messages.push(`${label}: ${number(n)}`);
  for(const message of messages)warnings.append(el('li','',message));
}
let loading=false;
async function refresh(){
  if(loading)return;loading=true;$('refresh').disabled=true;
  try{const response=await fetch('/api/usage?days='+$('range').value,{signal:AbortSignal.timeout(15000)});if(!response.ok)throw new Error(`Service returned ${response.status}`);render(await response.json());$('error').hidden=true;}
  catch(error){$('error').hidden=false;text('error',`Cannot refresh usage. Any numbers still shown are from the previous snapshot. ${error.message}`);}
  finally{loading=false;$('refresh').disabled=false;}
}
$('range').addEventListener('change',refresh);$('refresh').addEventListener('click',refresh);refresh();setInterval(refresh,60000);
