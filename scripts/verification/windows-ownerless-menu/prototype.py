"""Windows-only helper build/fresh isolated launch; no downloads/dependencies/product edits."""
import argparse, hashlib, json, os, pathlib, subprocess, zipfile
HERE=pathlib.Path(__file__).resolve().parent

def sha(p): return hashlib.sha256(p.read_bytes()).hexdigest()

def main():
    p=argparse.ArgumentParser()
    p.add_argument("command",choices=["build","launch","test"])
    p.add_argument("--studio",required=True,type=pathlib.Path)
    p.add_argument("--jbr",type=pathlib.Path,help="Defaults to the given Studio's bundled jbr")
    p.add_argument("--run",required=True,type=pathlib.Path,help="New private output/profile directory")
    a=p.parse_args()
    if os.name!="nt": raise SystemExit("Windows-only prototype")
    studio=a.studio.resolve(); jbr=(a.jbr or studio/"jbr").resolve(); run=a.run.resolve()
    run.mkdir(parents=True,exist_ok=True)
    product=json.loads((studio/"product-info.json").read_text(encoding="utf-8"))
    runtime=subprocess.check_output([str(jbr/"bin/java.exe"),"-version"],stderr=subprocess.STDOUT,text=True)
    if product["version"]!="AI-262.10968.63.2622.16730706" or "25.0.4+-16339643-b508.27" not in runtime:
        raise SystemExit("Fixed Studio/JBR prerequisite mismatch; no substitute")
    sdk_hashes={n:sha(studio/"lib"/n) for n in ["intellij.platform.ide.jar","intellij.platform.ide.impl.jar","intellij.platform.ide.core.jar"]}
    sdk_hashes["jbr/lib/modules"]=sha(jbr/"lib/modules")
    expected={
        "intellij.platform.ide.jar":"a6450f0f05c0fbec3db206b33e8ce5f2b1225985766b544d6ddbcc38af1ee74a",
        "intellij.platform.ide.impl.jar":"9083f5d8a907fdc4c7878db35d9d89d5a723ba46d4a0741e3ab83c07a940ab7b",
        "intellij.platform.ide.core.jar":"e9c187e429593a350c530c597cd3e756e186f325f3f5d5b015eccf379425f46c",
        "jbr/lib/modules":"5e6d316ecdb0ee7abec4b69433f1a6f51fd706be2a142b263fd675a284a5a6c5"}
    if sdk_hashes!=expected: raise SystemExit("Fixed input hashes differ")
    if a.command=="test":
        classes=run/"test-classes"
        if classes.exists(): raise SystemExit("Do not overwrite existing test output")
        classes.mkdir()
        sources=sorted((HERE/"src").rglob("*.java"))+sorted((HERE/"tests").rglob("*.java"))
        subprocess.run([str(jbr/"bin/javac.exe"),"-encoding","UTF-8","-cp",str(studio/"lib/*"),"-d",str(classes),*[str(s) for s in sources]],check=True)
        for test in ["MenuKeyBindingsTest", "MenuStateTest"]:
            subprocess.run([str(jbr/"bin/java.exe"),"-Djava.awt.headless=true","-Dprototype.trace="+str(run/"test-private.log"),
                "-Dprototype.state="+str(run/"test-state.json"),"-cp",str(classes)+os.pathsep+str(studio/"lib/*"),
                "privateacceptance.windowsmenu."+test],check=True)
        return
    if a.command=="build":
        classes=run/"classes"
        if classes.exists(): raise SystemExit("Do not overwrite an existing build")
        classes.mkdir()
        sources=sorted((HERE/"src").rglob("*.java"))
        subprocess.run([str(jbr/"bin/javac.exe"),"-encoding","UTF-8","-cp",str(studio/"lib/*"),"-d",str(classes),*[str(s) for s in sources]],check=True)
        jar=run/"windows-ownerless-menu-prototype.jar"
        with zipfile.ZipFile(jar,"x",compression=zipfile.ZIP_DEFLATED) as z:
            for base in [classes,HERE/"resources"]:
                for f in sorted(base.rglob("*")):
                    if f.is_file(): z.write(f,f.relative_to(base).as_posix())
        record={"studio_version":product["version"],"runtime":runtime,"inputs":sdk_hashes,"jar_sha256":sha(jar),
                "source_sha256":{str(f.relative_to(HERE)):sha(f) for f in HERE.rglob("*") if f.is_file()}}
        (run/"build-private.json").write_text(json.dumps(record,indent=2),encoding="utf-8")
        print("Built helper-only JAR",jar,"SHA256",sha(jar))
    else:
        profile=run/"profile"
        if profile.exists(): raise SystemExit("Fresh profile required; will not reuse/overwrite")
        plugin=profile/"plugins/windows-ownerless-menu/lib"; plugin.mkdir(parents=True)
        jar=run/"windows-ownerless-menu-prototype.jar"
        (plugin/jar.name).write_bytes(jar.read_bytes())
        home=run/"profile-home"; home.mkdir()
        project=run/"project"; (project/".idea").mkdir(parents=True)
        (project/".idea/.name").write_text("Windows独自menu試作・製品なし",encoding="utf-8")
        properties=run/"studio.properties"
        properties.write_text("\n".join("idea."+k+".path="+str(profile/v).replace("\\","/")
             for k,v in [("config","config"),("system","system"),("plugins","plugins"),("log","log")]),encoding="utf-8")
        options=(studio/"bin/studio64.exe.vmoptions").read_text(encoding="utf-8")
        options+="\n-Duser.home="+str(home)+"\n-Dprototype.trace="+str(run/"helper-private.log")+"\n-Dprototype.state="+str(run/"state.json")+"\n"
        vm=run/"studio.vmoptions"; vm.write_text(options,encoding="utf-8")
        env=os.environ.copy(); env.update(STUDIO_PROPERTIES=str(properties),STUDIO_VM_OPTIONS=str(vm),STUDIO_JDK=str(jbr))
        process=subprocess.Popen([str(studio/"bin/studio64.exe"),str(project)],env=env)
        (run/"launch-private.json").write_text(json.dumps({"pid":process.pid,"studio":str(studio),"jbr":str(jbr),"run":str(run)},indent=2),encoding="utf-8")
        print("Started fresh isolated helper-only Studio; private launch record saved")
if __name__=="__main__": main()
