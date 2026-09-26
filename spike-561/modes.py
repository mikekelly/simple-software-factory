import subprocess,os,json,time,select,base64,re
env={k:v for k,v in os.environ.items() if not k.startswith("HERDR_")}
p=subprocess.Popen(["herdr","terminal","session","control","wG:p1","--cols","100","--rows","30"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,env=env,bufsize=0)
def send(o): p.stdin.write((json.dumps(o)+"\n").encode())
def drain(s):
    out=b"";end=time.time()+s
    while time.time()<end:
        r,_,_=select.select([p.stdout],[],[],0.05)
        if r: out+=os.read(p.stdout.fileno(),1<<20)
    return b"".join(base64.b64decode(json.loads(l)["bytes"]) for l in out.splitlines() if b"terminal.frame" in l)
def modes(b): return sorted(set(re.findall(rb"\x1b\[\?[0-9;]+[hl]",b)))
drain(1)
send({"type":"terminal.input","text":"clear; printf '\\e[?2004h\\e[?1002h\\e[?1006h\\e[?1049h\\e[?1h'; cat -v\r"})
print("incremental:",modes(drain(1.5)))
send({"type":"terminal.resize","cols":101,"rows":30})
print("full repaint:",modes(drain(1.5)))
# mouse: SGR press at 5,5 as xterm would send it
send({"type":"terminal.input","bytes":base64.b64encode(b"\x1b[<0;5;5M\x1b[<0;5;5m\x1b[<64;5;5M").decode()})
send({"type":"terminal.input","text":"\r"})
print("cat -v saw:", re.findall(rb"\^\[\[<[0-9;]+[Mm]", drain(1)))
send({"type":"terminal.input","bytes":base64.b64encode(b"\x03").decode()})
send({"type":"terminal.input","text":"printf '\\e[?2004l\\e[?1002l\\e[?1006l\\e[?1049l\\e[?1l'; clear\r"})
print("after reset:",modes(drain(1.5)))
p.stdin.close(); p.wait()
