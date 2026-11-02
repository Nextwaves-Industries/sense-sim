//! The page around a report's content: its look, its print layout and the readout that
//! follows the pointer over a chart. Nothing loads from outside the file.

use super::esc;

const STYLE: &str = r#"
:root{color-scheme:light;--ink:#0b0b0b;--ink2:#52514e;--ink3:#6f6e68;--grid:#e1e0d9;--axis:#c3c2b7;--wash:#f6f6f3;--good:#006300;--bad:#b42323;--act:#256abf}
*{box-sizing:border-box}
body{font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif;color:var(--ink);background:#fff;margin:0 auto;max-width:1000px;padding:28px 20px 60px;-webkit-print-color-adjust:exact;print-color-adjust:exact}
h1{font-size:28px;line-height:1.2;margin:0}
h2{font-size:20px;margin:40px 0 10px;padding-top:12px;border-top:1px solid var(--grid)}
h3{font-size:15px;margin:22px 0 6px}
p{margin:6px 0}
a{color:var(--act)}
.m{color:var(--ink2);margin:4px 0 0}
.note{color:var(--ink2);font-size:13px}
.chip{display:inline-block;font-size:12px;font-weight:600;padding:1px 8px;border-radius:10px;border:1px solid var(--axis);color:var(--ink2);margin-right:6px;vertical-align:1px}
nav{margin:14px 0 0;font-size:13px;display:flex;flex-wrap:wrap;gap:4px 14px}
.banner{border:1px solid #fab219;background:#fff8e6;border-radius:6px;padding:8px 12px;margin:14px 0 0}
.verdict{font-weight:600;margin:10px 0}.verdict.good{color:var(--good)}.verdict.bad{color:var(--bad)}
.tiles{display:grid;grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:10px;margin:12px 0}
.tile{border:1px solid var(--grid);border-radius:6px;padding:8px 12px}
.tile b{display:block;font-size:20px;font-weight:600;line-height:1.3}
.tile span{display:block;font-size:12px;color:var(--ink2)}
.scroll{overflow-x:auto}
table{border-collapse:collapse;width:100%;font-size:14px;margin:6px 0 12px;font-variant-numeric:tabular-nums}
th,td{text-align:left;padding:4px 12px 4px 0;border-bottom:1px solid var(--grid);font-weight:400;vertical-align:top}
th{color:var(--ink2)}thead th{font-weight:600;border-bottom-color:var(--axis);white-space:nowrap}
tbody th{white-space:nowrap}
tr.hl td,tr.hl th{background:var(--wash)}
code{font-size:13px}
details{margin:8px 0}summary{cursor:pointer;color:var(--ink2);font-size:14px}
svg{display:block;width:100%;height:auto}
svg:focus{outline:2px solid var(--act);outline-offset:2px}
.chart{margin:10px 0}
.grid2{display:grid;grid-template-columns:repeat(auto-fit,minmax(380px,1fr));gap:8px 20px}
.polars{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:8px 12px}
.views{display:grid;grid-template-columns:minmax(280px,4fr) minmax(320px,6fr);gap:12px;align-items:start}
.smith,.pattern3d{max-width:460px}
.planes{display:grid;grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:12px}
.plane{margin:0}.plane figcaption{font-size:14px;font-weight:600}.plane img{display:block;width:100%;height:auto;border:1px solid var(--grid)}
.maps{display:grid;grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:12px}
.legend{display:flex;flex-wrap:wrap;gap:4px 16px;font-size:13px;color:var(--ink2);margin:8px 0}
.legend i{display:inline-block;width:12px;height:12px;border-radius:3px;margin-right:6px;vertical-align:-1px}
.key{display:flex;align-items:center;gap:8px;font-size:12px;color:var(--ink2);margin:6px 0}
.key i{display:block;width:180px;height:10px;border-radius:2px}
.g{stroke:var(--grid);stroke-width:1}.base{stroke:var(--axis);stroke-width:1}.mark{stroke:#898781;stroke-width:1}
.dim{stroke:#898781;stroke-width:1;fill:none}
.t{font-size:13px;fill:var(--ink);font-weight:600}.a{font-size:11px;fill:var(--ink3)}.vt{font-size:12px;fill:var(--ink2);font-weight:600}
.ring{font-size:9.5px;fill:var(--ink3)}.pn{font-size:10px;font-weight:700;fill:#fff;text-anchor:middle}
.hair{stroke:var(--ink2);stroke-width:1;pointer-events:none}
.tip{position:fixed;z-index:9;background:#fff;border:1px solid var(--axis);border-radius:6px;box-shadow:0 2px 10px rgba(11,11,11,.14);padding:6px 10px;font-size:12px;pointer-events:none;max-width:280px}
.tip div.h{color:var(--ink2);margin-bottom:2px}
.tip div.r{white-space:nowrap}.tip i{display:inline-block;width:12px;height:2px;margin-right:6px;vertical-align:3px}
.tip b{font-weight:600;margin-right:6px}.tip span{color:var(--ink2)}
@media (max-width:760px){.views{grid-template-columns:1fr}}
@page{size:A4;margin:14mm}
@media print{body{max-width:none;padding:0;font-size:12px}nav,.tip{display:none}table{font-size:11px}h2{break-after:avoid;margin-top:20px}svg,figure,tr,.tile{break-inside:avoid}.scroll{overflow:visible}summary{display:none}a[download]{display:none}}
"#;

const SCRIPT: &str = r#"
(function(){
var tip=document.createElement('div');tip.className='tip';tip.hidden=true;document.body.appendChild(tip);
function show(cx,cy,head,rows){
  tip.textContent='';
  if(head){var h=document.createElement('div');h.className='h';h.textContent=head;tip.appendChild(h);}
  rows.forEach(function(r){
    var d=document.createElement('div');d.className='r';
    if(r.c){var k=document.createElement('i');k.style.background=r.c;d.appendChild(k);}
    var b=document.createElement('b');b.textContent=r.v;d.appendChild(b);
    var s=document.createElement('span');s.textContent=r.n;d.appendChild(s);
    tip.appendChild(d);
  });
  tip.hidden=false;
  var w=tip.offsetWidth,t=tip.offsetHeight,x=cx+14,y=cy+14;
  if(x+w>innerWidth-8)x=cx-w-14;
  if(y+t>innerHeight-8)y=cy-t-14;
  tip.style.left=Math.max(4,x)+'px';tip.style.top=Math.max(4,y)+'px';
}
function hide(){tip.hidden=true;}
function toSvg(svg,e){var p=svg.createSVGPoint();p.x=e.clientX;p.y=e.clientY;return p.matrixTransform(svg.getScreenCTM().inverse());}
function toScreen(svg,x,y){var p=svg.createSVGPoint();p.x=x;p.y=y;return p.matrixTransform(svg.getScreenCTM());}
function nearest(xs,x){var a=0,b=xs.length-1;while(b-a>1){var m=(a+b)>>1;if(xs[m]<x)a=m;else b=m;}return x-xs[a]<xs[b]-x?a:b;}
function mk(svg,name,cls){var e=document.createElementNS('http://www.w3.org/2000/svg',name);e.setAttribute('class',cls);e.style.display='none';svg.appendChild(e);return e;}
document.querySelectorAll('svg[data-hover]').forEach(function(svg){
  var d;try{d=JSON.parse(svg.getAttribute('data-hover'));}catch(e){return;}
  if(d.pts){
    if(!d.pts.length)return;
    var ring=mk(svg,'circle','hair');ring.setAttribute('r','5');ring.setAttribute('fill','none');
    var at=0;
    var pick=function(i,px,py){
      at=i;var q=d.pts[i];
      ring.setAttribute('cx',q[0]);ring.setAttribute('cy',q[1]);ring.style.display='';
      var s=toScreen(svg,q[0],q[1]);
      show(px===undefined?s.x:px,py===undefined?s.y:py,'',[{v:q[2],n:q[3],c:q[4]}]);
    };
    svg.addEventListener('pointermove',function(e){
      var p=toSvg(svg,e),best=-1,dist=900;
      d.pts.forEach(function(q,i){var dx=q[0]-p.x,dy=q[1]-p.y,g=dx*dx+dy*dy;if(g<dist){dist=g;best=i;}});
      if(best<0){ring.style.display='none';hide();}else pick(best,e.clientX,e.clientY);
    });
    svg.addEventListener('pointerleave',function(){ring.style.display='none';hide();});
    svg.addEventListener('focus',function(){pick(at);});
    svg.addEventListener('blur',function(){ring.style.display='none';hide();});
    svg.addEventListener('keydown',function(e){
      if(e.key==='ArrowRight'||e.key==='ArrowLeft'){e.preventDefault();pick((at+(e.key==='ArrowRight'?1:d.pts.length-1))%d.pts.length);}
    });
    return;
  }
  var first=d.s[0];if(!first||!first.y.length)return;
  var xs=first.x,hair=mk(svg,'line','hair');
  hair.setAttribute('y1',d.m[2]);hair.setAttribute('y2',d.m[3]);
  var at=0;
  var pos=function(x){return d.m[0]+(x-d.xr[0])/((d.xr[1]-d.xr[0])||1)*(d.m[1]-d.m[0]);};
  var pick=function(i,cx,cy){
    at=i;var x=xs[i],sx=pos(x);
    hair.setAttribute('x1',sx);hair.setAttribute('x2',sx);hair.style.display='';
    var rows=d.s.map(function(s){
      var sxs=s.x||xs,j=(s.x?nearest(sxs,x):i);
      return {v:String((+s.y[j]).toFixed(2)).replace('-','−')+d.u,n:s.n,c:s.c};
    });
    var s=toScreen(svg,sx,(d.m[2]+d.m[3])/2);
    show(cx===undefined?s.x:cx,cy===undefined?s.y:cy,x+' '+d.xl.replace(/^.*\(|\)$/g,''),rows);
  };
  svg.addEventListener('pointermove',function(e){
    var p=toSvg(svg,e);
    if(p.x<d.m[0]-8||p.x>d.m[1]+8||p.y<d.m[2]-8||p.y>d.m[3]+8){hair.style.display='none';hide();return;}
    var x=d.xr[0]+(p.x-d.m[0])/((d.m[1]-d.m[0])||1)*(d.xr[1]-d.xr[0]);
    pick(nearest(xs,x),e.clientX,e.clientY);
  });
  svg.addEventListener('pointerleave',function(){hair.style.display='none';hide();});
  svg.addEventListener('focus',function(){pick(at);});
  svg.addEventListener('blur',function(){hair.style.display='none';hide();});
  svg.addEventListener('keydown',function(e){
    if(e.key==='ArrowRight'&&at<xs.length-1){e.preventDefault();pick(at+1);}
    if(e.key==='ArrowLeft'&&at>0){e.preventDefault();pick(at-1);}
  });
});
window.addEventListener('beforeprint',function(){document.querySelectorAll('details').forEach(function(x){x.open=true;});});
})();
"#;

/// A whole page with this body.
pub fn wrap(title: &str, body: &str) -> String {
    format!("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{STYLE}</style></head><body>{body}<script>{SCRIPT}</script></body></html>", esc(title))
}
