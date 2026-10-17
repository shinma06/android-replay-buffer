package io.github.shinma06.replaybuffer.core;


import java.nio.*;

import java.nio.channels.*;

import java.nio.file.*;

import java.lang.reflect.*;

import java.io.*;

import java.util.*;

import java.util.concurrent.*;

import kotlin.jvm.functions.Function1;

import org.junit.jupiter.api.Test;

import org.junit.jupiter.api.io.TempDir;

import static org.junit.jupiter.api.Assertions.*;


class JournalTransitionTest {
 @TempDir Path root;

 static Object get(Object o,String n)throws Exception{Field f=o.getClass().getDeclaredField(n);
f.setAccessible(true);
return f.get(o);
}
 static void set(Object o,String n,Object v)throws Exception{Field f=o.getClass().getDeclaredField(n);
f.setAccessible(true);
f.set(o,v);
}
 static Object ctor(Class<?> c,int n,Object...v)throws Exception{return Arrays.stream(c.getConstructors()).filter(x->x.getParameterCount()==n).findFirst().orElseThrow().newInstance(v);
}
 static Object entry(Object old,FileChannel ch,FileLock lock)throws Exception{Constructor<?> t=Arrays.stream(old.getClass().getDeclaredConstructors()).filter(x->x.getParameterCount()==5).findFirst().orElseThrow();
t.setAccessible(true);
return t.newInstance(get(old,"record"),ch,lock,get(old,"fileKey"),get(old,"hostPending"));
}
 static FileChannel create(Path p)throws IOException{return FileChannel.open(p,StandardOpenOption.CREATE_NEW,StandardOpenOption.READ,StandardOpenOption.WRITE,LinkOption.NOFOLLOW_LINKS);
}
 static class CutChannel extends FileChannel {
  final FileChannel real;
int remaining;
CutChannel(FileChannel r,int n){real=r;
remaining=n;
}

  public int write(ByteBuffer b)throws IOException{if(remaining==0)throw new IOException("CONTROLLED partial-write failure");
int limit=b.limit();
b.limit(Math.min(limit,b.position()+remaining));
int n=real.write(b);
b.limit(limit);
remaining-=n;
return n;
}

  public long write(ByteBuffer[] b,int o,int n)throws IOException{throw new UnsupportedOperationException();
}

  public int read(ByteBuffer b)throws IOException{return real.read(b);
}
  public long read(ByteBuffer[]b,int o,int n)throws IOException{return real.read(b,o,n);
}

  public long position()throws IOException{return real.position();
}
  public FileChannel position(long p)throws IOException{real.position(p);
return this;
}
  public long size()throws IOException{return real.size();
}
  public FileChannel truncate(long p)throws IOException{real.truncate(p);
return this;
}public void force(boolean m)throws IOException{real.force(m);
}
  public long transferTo(long p,long n,WritableByteChannel c)throws IOException{return real.transferTo(p,n,c);
}
  public long transferFrom(ReadableByteChannel c,long p,long n)throws IOException{return real.transferFrom(c,p,n);
}
  public int read(ByteBuffer b,long p)throws IOException{return real.read(b,p);
}
  public int write(ByteBuffer b,long p)throws IOException{return real.write(b,p);
}public MappedByteBuffer map(MapMode m,long p,long n)throws IOException{return real.map(m,p,n);
}
  public FileLock lock(long p,long n,boolean s)throws IOException{return real.lock(p,n,s);
}
  public FileLock tryLock(long p,long n,boolean s)throws IOException{return real.tryLock(p,n,s);
}
  protected void implCloseChannel()throws IOException{real.close();
}
 }
 public static class ReleaseFailure extends FileLock {
  final FileLock real;
public ReleaseFailure(FileLock r){super(r.channel(),r.position(),r.size(),r.isShared());
real=r;
}
  public boolean isValid(){return real.isValid();
}public void release()throws IOException{throw new IOException("CONTROLLED post-delete lock-release failure");
}
 }

 static class CloseFailure extends CutChannel {
  CloseFailure(FileChannel r){super(r,Integer.MAX_VALUE);
}

  protected void implCloseChannel()throws IOException{real.close();
throw new IOException("CONTROLLED channel-close failure");
}
 }

 @Test void partialWriteKeepsCompleteOldRecordAndSuccessfulRetryTransfersLockedInode() throws Exception {
  Path r=Files.createDirectory(root.resolve("rewrite")).toRealPath();

  RemoteCleanup record=new RemoteCleanup("fixture-1","11111111111111111111111111111111",Map.of("23456","localabstract:scrcpy_00000001"),Set.of());

  RemoteCleanupJournal j=new RemoteCleanupJournal(r);
j.retain(record,true);

  Path path=r.resolve("remote-cleanup-"+record.getToken()+".json");
byte[] before=Files.readAllBytes(path);
Object oldKey=Files.readAttributes(path,java.nio.file.attribute.BasicFileAttributes.class).fileKey();

  Function1<Path,FileChannel> normal=p->{try{return create(p);
}catch(IOException e){throw new java.io.UncheckedIOException(e);
}};

  set(j,"createChannel",(Function1<Path,FileChannel>)p->{try{return new CutChannel(create(p),17);
}catch(IOException e){throw new java.io.UncheckedIOException(e);
}});

  assertThrows(IOException.class,()->j.retain(record,false));

  assertArrayEquals(before,Files.readAllBytes(path));
assertEquals(oldKey,Files.readAttributes(path,java.nio.file.attribute.BasicFileAttributes.class).fileKey());
assertFalse(j.getHasRecoverable());

  RemoteCleanupJournal competitor=new RemoteCleanupJournal(r);
competitor.recover();
assertEquals(1,competitor.getPendingCount());
assertFalse(competitor.getHasRecoverable());
competitor.close();

  set(j,"createChannel",normal);

  Map<Path,Object> owned=(Map<Path,Object>)get(j,"owned");
Object previous=owned.get(path);
FileChannel previousChannel=(FileChannel)get(previous,"channel");
owned.put(path,entry(previous,previousChannel,new ReleaseFailure((FileLock)get(previous,"lock"))));

  assertThrows(IOException.class,()->j.retain(record,false));
assertFalse(previousChannel.isOpen());

  assertNotEquals(oldKey,Files.readAttributes(path,java.nio.file.attribute.BasicFileAttributes.class).fileKey());
assertTrue(j.getHasRecoverable());

  competitor=new RemoteCleanupJournal(r);
competitor.recover();
assertEquals(1,competitor.getPendingCount());
assertFalse(competitor.getHasRecoverable());
competitor.close();

  j.retain(record,false);
j.close();
RemoteCleanupJournal next=new RemoteCleanupJournal(r);
next.recover();
assertEquals(1,next.getPendingCount());
assertTrue(next.getHasRecoverable());
next.close();

 }

 @Test void failedPublicationPreservesForeignReplacementAndOldLockedRecord() throws Exception {
  Path r=Files.createDirectory(root.resolve("foreign")).toRealPath();
RemoteCleanup record=new RemoteCleanup("fixture-1","33333333333333333333333333333333",Map.of(),Set.of());
RemoteCleanupJournal j=new RemoteCleanupJournal(r);
j.retain(record,true);

  Path path=r.resolve("remote-cleanup-"+record.getToken()+".json");
Path old=r.resolve("old-record");
byte[] before=Files.readAllBytes(path);

  set(j,"createChannel",(Function1<Path,FileChannel>)p->{try{FileChannel channel=create(p);
Files.move(path,old);
Files.writeString(path,"foreign");
return channel;
}catch(IOException e){throw new java.io.UncheckedIOException(e);
}});

  assertThrows(IllegalStateException.class,()->j.retain(record,false));
assertEquals("foreign",Files.readString(path));
assertArrayEquals(before,Files.readAllBytes(old));
assertFalse(j.getHasRecoverable());
j.close();

 }

 @Test void detectedStageReplacementSurvivesInitialAndUpdateFailureAndRetry() throws Exception {
  for (boolean update : new boolean[]{false, true}) {
   Path r=Files.createDirectory(root.resolve(update ? "stage-update" : "stage-initial")).toRealPath();
   RemoteCleanup record=new RemoteCleanup("fixture-1",update ? "55555555555555555555555555555555" : "66666666666666666666666666666666",Map.of(),Set.of());
   RemoteCleanupJournal j=new RemoteCleanupJournal(r);
   Path descriptor=r.resolve("remote-cleanup-"+record.getToken()+".json");
   if(update) j.retain(record,true);
   byte[] oldBytes=update ? Files.readAllBytes(descriptor) : null;
   Object oldKey=update ? Files.readAttributes(descriptor,java.nio.file.attribute.BasicFileAttributes.class).fileKey() : null;
   Object oldEntry=update ? ((Map<?,?>)get(j,"owned")).get(descriptor) : null;
   FileLock oldLock=update ? (FileLock)get(oldEntry,"lock") : null;
   Path[] stage={null}; FileChannel[] channel={null}; FileLock[] lock={null};
   Path moved=r.resolve("moved-owned-stage");
   set(j,"createChannel",(Function1<Path,FileChannel>)p->{try {
    stage[0]=p; channel[0]=create(p);
    return new CutChannel(channel[0],Integer.MAX_VALUE) {
     public FileLock tryLock(long position,long size,boolean shared)throws IOException {
      lock[0]=super.tryLock(position,size,shared);return lock[0];
     }
     public void force(boolean metadata)throws IOException {
      super.force(metadata);assertTrue(lock[0].isValid());Files.move(p,moved);
      Files.writeString(p,"foreign",StandardOpenOption.CREATE_NEW);
     }
    };
   }catch(IOException e){throw new java.io.UncheckedIOException(e);}});
   assertThrows(IllegalStateException.class,()->j.retain(record,false));
   assertEquals("foreign",Files.readString(stage[0]));
   assertTrue(Files.readString(moved).contains(record.getToken()));
   assertFalse(channel[0].isOpen());assertFalse(lock[0].isValid());
   if(update) {
    assertArrayEquals(oldBytes,Files.readAllBytes(descriptor));
    assertEquals(oldKey,Files.readAttributes(descriptor,java.nio.file.attribute.BasicFileAttributes.class).fileKey());
    assertTrue(oldLock.isValid());assertFalse(j.getHasRecoverable());
   }else assertFalse(Files.exists(descriptor));
   Path foreign=stage[0];
   set(j,"createChannel",(Function1<Path,FileChannel>)p->{try {stage[0]=p;return create(p);}catch(IOException e){throw new java.io.UncheckedIOException(e);}});
   j.retain(record,false);assertFalse(Files.exists(stage[0]));assertEquals("foreign",Files.readString(foreign));
   j.close();RemoteCleanupJournal next=new RemoteCleanupJournal(r);next.recover();assertTrue(next.getHasRecoverable());next.close();
  }
 }

 @Test void atomicPublicationFailureKeepsOldBytesAndRetryCanRecover() throws Exception {
  Path r=Files.createDirectory(root.resolve("move-failure")).toRealPath();
RemoteCleanup record=new RemoteCleanup("fixture-1","44444444444444444444444444444444",Map.of(),Set.of());
RemoteCleanupJournal j=new RemoteCleanupJournal(r);
j.retain(record,true);

  Path path=r.resolve("remote-cleanup-"+record.getToken()+".json");
byte[] before=Files.readAllBytes(path);

  set(j,"createChannel",(Function1<Path,FileChannel>)p->{try{return new CutChannel(create(p),Integer.MAX_VALUE){public void force(boolean metadata)throws IOException{super.force(metadata);
Files.setPosixFilePermissions(r,java.nio.file.attribute.PosixFilePermissions.fromString("r-x------"));
}};
}catch(IOException e){throw new java.io.UncheckedIOException(e);
}});

  try {assertThrows(IOException.class,()->j.retain(record,false));
assertArrayEquals(before,Files.readAllBytes(path));
assertFalse(j.getHasRecoverable());
}
  finally {Files.setPosixFilePermissions(r,java.nio.file.attribute.PosixFilePermissions.fromString("rwx------"));
}
  set(j,"createChannel",(Function1<Path,FileChannel>)p->{try{return create(p);
}catch(IOException e){throw new java.io.UncheckedIOException(e);
}});
j.retain(record,false);
j.close();
RemoteCleanupJournal next=new RemoteCleanupJournal(r);
next.recover();
assertTrue(next.getHasRecoverable());
next.close();

 }

 @Test void deleteCommitsRetirementAndBothCleanupFailuresRemainRetryable() throws Exception {
  Path r=Files.createDirectory(root.resolve("retire")).toRealPath();
RemoteCleanup record=new RemoteCleanup("fixture-1","22222222222222222222222222222222",Map.of(),Set.of());
RemoteCleanupJournal j=new RemoteCleanupJournal(r);
j.retain(record,false);
Path path=r.resolve("remote-cleanup-"+record.getToken()+".json");
Map<Path,Object> map=(Map<Path,Object>)get(j,"owned");
Object old=map.get(path);
FileLock real=(FileLock)get(old,"lock");
FileChannel ch=(FileChannel)get(old,"channel");
map.put(path,entry(old,new CloseFailure(ch),new ReleaseFailure(real)));

  IOException failure=assertThrows(IOException.class,()->j.retire(record));
assertEquals("CONTROLLED post-delete lock-release failure",failure.getMessage());
assertEquals("CONTROLLED channel-close failure",failure.getSuppressed()[0].getMessage());
assertFalse(ch.isOpen());
assertFalse(Files.exists(path));
assertEquals(0,j.getPendingCount());
j.retire(record);

  Files.writeString(path,"foreign");
j.retire(record);
assertEquals("foreign",Files.readString(path));
Files.delete(path);
j.retain(record,false);
j.retire(record);
assertEquals(0,j.getPendingCount());
j.close();

 }
 @Test void allocationCapacityIsCheckedBeforeTheSixtyFifthSideEffect() throws Exception {
  Path r=root.resolve("capacity");

  Files.createDirectories(r);
Path fake=r.resolve("fake-adb");
Files.writeString(fake,"#!/usr/bin/python3\nimport sys,pathlib\nr=pathlib.Path(__file__).parent;a=sys.argv[1:];p=r/'mappings'\nif a==['devices','-l']: print('List of devices attached')\nelif a==['forward','--list']: print(p.read_text() if p.exists() else '',end='')\nelif a[:3]==['-s','fixture-1','push']: pass\nelif a[:4]==['-s','fixture-1','forward','tcp:0']:\n lines=p.read_text().splitlines() if p.exists() else []; lines.append('fixture-1 tcp:'+str(20000+len(lines))+' '+a[4]);p.write_text('\\n'.join(lines)+'\\n');sys.exit(1)\nelse: sys.exit(1)\n");
fake.toFile().setExecutable(true);

  CaptureResources resources=(CaptureResources)ctor(CaptureResources.class,4,Files.createDirectory(r.resolve("resources")),null,2,null);
CaptureStore store=(CaptureStore)ctor(CaptureStore.class,11,r.resolve("ring"),null,0L,0L,0L,null,null,null,null,510,null);
ApplicationTarget app=(ApplicationTarget)ctor(ApplicationTarget.class,5,null,null,null,7,null);
DeviceCapture cap=new DeviceCapture(fake,"fixture-1",resources,store,1,app);
ReplaySettings settings=new ReplaySettings(fake,null,60,app,CaptureMode.VIDEO_AND_LOGS,false);
ReplayCore core=new ReplayCore(settings,r.resolve("workspace"),r.resolve("journal"));
core.applySettings(settings).get(5,TimeUnit.SECONDS);
ScheduledThreadPoolExecutor control=(ScheduledThreadPoolExecutor)get(get(core,"control"),"e");
control.getQueue().clear();
set(core,"backend",cap);
set(core,"enabled",true);
set(core,"store",store);
set(core,"resources",resources);
set(core,"probe",new OwnedAdb(fake));

  try{Method video=DeviceCapture.class.getDeclaredMethod("video");
video.setAccessible(true);
for(int i=0;
i<65;
i++){try{video.invoke(cap);
throw new AssertionError("allocation should fail");
}catch(InvocationTargetException expected){if(!(expected.getCause() instanceof IllegalStateException))throw expected;
}}
   int pending=cap.cleanupRecord().getPendingEndpoints().size();
assertEquals(64,pending);
assertEquals(64,Files.readAllLines(r.resolve("mappings")).size());
ReplayOperation first=core.setEnabled(false).get(10,TimeUnit.SECONDS);
ReplayOperation second=core.setEnabled(false).get(10,TimeUnit.SECONDS);
assertTrue(first.getAccepted());
assertTrue(second.getAccepted());
assertNull(get(core,"backend"));
Path descriptor=r.resolve("journal/remote-cleanup-"+cap.cleanupRecord().getToken()+".json");
assertTrue(Files.exists(descriptor));
String json=Files.readString(descriptor);
for(String endpoint:cap.cleanupRecord().getPendingEndpoints())assertTrue(json.contains(endpoint));
assertEquals(1,core.snapshot().getCleanupPendingCount());


  }finally{
   core.closeAsync().get(10,TimeUnit.SECONDS);
System.out.println("CAPACITY_FIXTURE_CLEANUP owned_core_closed=true");
}
 }
}
