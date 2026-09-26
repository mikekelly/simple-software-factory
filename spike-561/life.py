import subprocess,os,json,time,select
env={k:v for k,v in os.environ.items() if not k.startswith("HERDR_")}
p=subprocess.Popen(["herdr","terminal","session","control","wF:p1"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env)
time.sleep(1)
# kill the shell in the pane: what does the controller see?
subprocess.run(["herdr","pane","send-text","wF:p1","exit\r"],env=env)
time.sleep(2)
r,_,_=select.select([p.stdout],[],[],0.1)
out=os.read(p.stdout.fileno(),1<<20) if r else b""
print([json.loads(l)["type"]+":"+str(json.loads(l).get("reason")) for l in out.splitlines()][-3:], "exit", p.poll(), p.stderr.read() if p.poll() is not None else "")
q=subprocess.run(["herdr","terminal","session","control","wF:p1"],input=b"",capture_output=True,env=env,timeout=5); print("reattach to gone pane:", q.returncode, q.stdout[-200:], q.stderr[-300:])
