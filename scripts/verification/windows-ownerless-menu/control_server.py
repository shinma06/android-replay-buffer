"""Loopback-only safe foreground page, DOM key leakage counter and dummy callback display."""
import argparse, http.server, json, pathlib

PAGE="""<!doctype html><meta charset="utf-8"><title>Chrome安全control — Windows独自menu診断</title>
<style>body{margin:45px;background:#102c45;color:white;font:24px "Segoe UI",sans-serif}
h1{font-size:35px}pre{white-space:pre-wrap;background:#193d59;padding:25px}
.ok{color:#93f4b0}#leak{font-size:30px}footer{margin-top:45px;font-size:20px}</style>
<h1>Chrome前面 control / 安全・入力フォームなし</h1>
<p>実tray Wからmenu。IDE本体は背面のままを期待。</p>
<p id="leak" class="ok">Chrome受信キー = 0</p><pre id="state">helper準備待ち</pre>
<p id="focus">page focus</p><footer>外側クリック領域 — 入力・送信・外部リンクなし<br>
診断だけ。実capture／保存／USB／物理音声なし。</footer>
<script>
let keys=[]; window.addEventListener('keydown',e=>{
 keys.push(e.key); const el=document.getElementById('leak');
 el.textContent='Chrome受信キー = '+keys.length+' : '+keys.join(', ');
 el.style.color='#ff8d8d'; e.preventDefault();
});
setInterval(async()=>{try{
 const d=await (await fetch('/state',{cache:'no-store'})).json();
 document.getElementById('state').textContent=
  'dummy callback = '+d.callbacks+'\\nlast = '+d.last+
  '\\nchecked = '+JSON.stringify(d.checked)+'\\nopened / closed = '+d.shows+' / '+d.closes+
  '\\n実menu window = '+d.menuWindows+'\\nhelper終了 = '+d.stopped;
}catch(e){}
document.getElementById('focus').textContent='Chrome page focus = '+document.hasFocus();
},300);
</script>"""
def main():
    p=argparse.ArgumentParser(); p.add_argument("--state",required=True,type=pathlib.Path); p.add_argument("--port",type=int,default=8767); a=p.parse_args()
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path=="/state":
                try: data=a.state.read_bytes(); json.loads(data)
                except (OSError,ValueError): data=b'{}'
                mime="application/json"
            elif self.path in ["/","/index.html"]: data=PAGE.encode("utf-8"); mime="text/html; charset=utf-8"
            else: self.send_error(404); return
            self.send_response(200); self.send_header("Content-Type",mime); self.send_header("Cache-Control","no-store"); self.end_headers(); self.wfile.write(data)
        def log_message(self,*args): pass
    server=http.server.ThreadingHTTPServer(("127.0.0.1",a.port),Handler)
    print("Safe control page ready on loopback port",a.port,flush=True)
    try: server.serve_forever()
    except KeyboardInterrupt: pass
    finally: server.server_close()
if __name__=="__main__": main()
