import subprocess, json, base64, time, sys, os, select, threading
P="wF:p1"
env={k:v for k,v in os.environ.items() if not k.startswith("HERDR_")}
def start(mode="control", extra=()):
    return subprocess.Popen(["herdr","terminal","session",mode,P,*extra],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env,bufsize=0)
def send(p,o): p.stdin.write((json.dumps(o)+"\n").encode()); p.stdin.flush()
def drain(p, secs):
    frames=[]; buf=b""; end=time.time()+secs
    while time.time()<end:
        r,_,_=select.select([p.stdout],[],[],0.05)
        if r:
            d=os.read(p.stdout.fileno(),1<<20)
            if not d: break
            buf+=d
            while b"\n" in buf:
                l,buf=buf.split(b"\n",1); frames.append((time.time(),json.loads(l)))
    return frames
def summ(fr):
    f=[x for _,x in fr if x["type"]=="terminal.frame"]
    return dict(n=len(f), full=sum(1 for x in f if x.get("full")), bytes=sum(len(base64.b64decode(x["bytes"])) for x in f), sizes={(x["width"],x["height"]) for x in f}, other=[x for _,x in fr if x["type"]!="terminal.frame"])
test=sys.argv[1]
p=start("control",["--cols","100","--rows","30"])
print("initial",summ(drain(p,1)))
if test=="flood":
    send(p,{"type":"terminal.input","text":"seq 1 300000\r"})
    t=time.time(); fr=drain(p,8); s=summ(fr); print("flood",s,"span",fr[-1][0]-t if fr else 0)
    ts=[a for a,x in fr if x["type"]=="terminal.frame"]; 
    if len(ts)>1: print("frame interval ms min/median", round(min(b-a for a,b in zip(ts,ts[1:]))*1000,1), round(sorted(b-a for a,b in zip(ts,ts[1:]))[len(ts)//2]*1000,1))
if test=="stall":
    send(p,{"type":"terminal.input","text":"seq 1 3000000\r"})
    time.sleep(10)  # don't read: pipe fills
    fr=drain(p,6); print("after stall",summ(fr))
    print("last frame decodes:", base64.b64decode([x for _,x in fr if x["type"]=="terminal.frame"][-1]["bytes"])[-200:])
if test=="scroll":
    send(p,{"type":"terminal.input","text":"seq 1 500\r"}); drain(p,1.5)
    send(p,{"type":"terminal.scroll","lines":20,"direction":"up"}); print("up",summ(drain(p,0.7)))
    send(p,{"type":"terminal.scroll","lines":10,"direction":"down"}); print("down",summ(drain(p,0.7)))
    send(p,{"type":"terminal.scroll","lines":10,"direction":"sideways"}); print("bad",summ(drain(p,0.7)))
    send(p,{"type":"terminal.input","text":"x"}); print("type",summ(drain(p,0.7)))
if test=="resize":
    send(p,{"type":"terminal.resize","cols":90,"rows":25}); print("resize",summ(drain(p,1)))
    print(subprocess.run(["herdr","pane","layout","--pane",P],capture_output=True,text=True,env=env).stdout[:600])
    o=start("observe"); print("observe default", summ(drain(o,1))); o.stdin.close()
    o=start("observe",["--cols","90","--rows","25"]); print("observe sized", summ(drain(o,1))); o.stdin.close()
if test=="paste":
    send(p,{"type":"terminal.input","text":"printf '\\e[?2004h'; cat -v\r"}); drain(p,1)
    send(p,{"type":"terminal.input","bytes":base64.b64encode(b"\x1b[200~a\nb\x1b[201~\x1b\r\x1bb").decode()}); fr=drain(p,1)
    print(b"".join(base64.b64decode(x["bytes"]) for _,x in fr if x["type"]=="terminal.frame")[-400:])
    send(p,{"type":"terminal.input","bytes":base64.b64encode(b"\x03").decode()})
    send(p,{"type":"terminal.input","text":"printf '\\e[?2004l'; clear\r"}); drain(p,1)
p.stdin.close(); print("close",summ(drain(p,1)), p.wait(timeout=3))
