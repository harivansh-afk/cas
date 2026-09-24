export type Box = { x: number; y: number; w: number; h?: number; label: string; sub?: string; tone?: 'focus' | 'muted'; dashed?: boolean };
export type Edge = { d: string; focus?: boolean; dashed?: boolean };
export type Label = { x: number; y: number; text: string; focus?: boolean };
export type Panel = { title: string; description: string; h: number; boxes: Box[]; edges: Edge[]; labels?: Label[] };
const box = (x: number, y: number, w: number, label: string, sub?: string, tone?: Box['tone']): Box => ({ x, y, w, label, sub, tone });
export const architecture: Panel[] = [
 { title: 'Guest writes', description: 'QEMU shares request descriptors with CAS. Admission reserves capacity before the write enters the per-image write-ahead log. FLUSH makes the ordered log durable.', h: 254,
  boxes: [box(20,24,130,'QEMU guest','virtual block disk'),box(210,24,130,'CAS frontend','shared descriptors'),box(210,130,130,'Admission','reserve capacity'),box(20,130,130,'Image WAL','append writes','focus')],
  edges: [{d:'M150 50 H210'},{d:'M275 76 V130'},{d:'M210 156 H150'}],
  labels:[{x:180,y:105,text:'request'},{x:180,y:216,text:'WRITE publishes; FLUSH persists.'}]
 },
 { title: 'Background compaction', description: 'The compactor hashes durable log data, stores missing chunks, and commits a manifest. Covered log data can be reclaimed only after reader and replay pins release.', h:254,
  boxes:[box(20,24,130,'Durable WAL','fixed 4 KiB chunks'),box(210,24,130,'Shared store','write missing hashes','focus'),box(210,130,130,'Manifest','commit block → hash'),box(20,130,130,'Reclaim WAL','wait for pins')],
  edges:[{d:'M150 50 H210'},{d:'M275 76 V130'},{d:'M210 156 H150'}],
  labels:[{x:180,y:105,text:'sync chunks'},{x:180,y:216,text:'Freed space wakes admission.'}]
 }
];
export const reads: Panel[] = [
 {title:'A recently written block',description:'A captured read plan first checks the write-ahead log. Recent mappings take precedence over the manifest.',h:196,
 boxes:[box(20,35,130,'Read block','captured read plan'),box(210,35,130,'WAL overlay','newest mapping','focus')],edges:[{d:'M150 61 H210'}],labels:[{x:180,y:132,text:'Recent data comes from the log.'}]},
 {title:'A block outside the WAL overlay',description:'The manifest maps the block to a content hash. The shared cache serves a hit; a miss loads and verifies the chunk from the store. Holes return zeros.',h:196,
 boxes:[box(10,35,100,'Manifest','block → hash'),box(130,35,100,'Cache','shared chunks'),box(250,35,100,'Store','load + verify','focus')],edges:[{d:'M110 61 H130'},{d:'M230 61 H250'}],labels:[{x:242,y:25,text:'miss'},{x:180,y:132,text:'One request can read both sources.'}]}
];
export const capacity: Panel[] = [
 {title:'Before · a full log becomes an error',description:'A write reaches a full WAL quota. The admission deadline expires after five seconds and returns an IO error to the guest.',h:218,
 boxes:[box(20,30,130,'Write','WAL quota full'),box(210,30,130,'Wait','5 s deadline'),box(210,132,130,'Guest IOERR',undefined,'focus')],edges:[{d:'M150 56 H210'},{d:'M275 82 V132'}],labels:[{x:92,y:163,text:'space is still full'}]},
 {title:'After · wait for capacity',description:'The request stays pending and its payload stays in guest RAM. WAL reclamation restores capacity and wakes admission. A periodic retry covers releases without notifications.',h:218,
 boxes:[box(20,30,130,'Pending write','payload in guest RAM','focus'),box(210,30,130,'Reclaim WAL','space restored'),box(210,132,130,'Retry admission','continue write')],edges:[{d:'M275 82 V132',focus:true}],labels:[{x:292,y:111,text:'wake'},{x:92,y:163,text:'no timeout error'}]}
];
export const compaction: Panel[] = [
 {title:'Before · repeat old work',description:'Each selection walks retained WAL prefixes. Preparation initializes maximum buffers and final output includes intermediate manifest pages.',h:238,
 boxes:[box(55,20,250,'Scan retained WAL','old prefix + new records'),box(55,102,250,'Prepare and emit','full buffers + intermediate pages')],edges:[{d:'M180 72 V102'},{d:'M55 128 H22 V46 H55',dashed:true}],labels:[{x:180,y:202,text:'Repeat for the next batch.'}]},
 {title:'After · keep the useful work',description:'Selection resumes from a validated cursor. Buffers grow as needed. Final manifest output retains only reachable new pages. Reclamation still walks history.',h:238,
 boxes:[box(55,20,250,'Resume selection','validated WAL cursor','focus'),box(55,102,250,'Prepare and emit','grow buffers + final pages')],edges:[{d:'M180 72 V102',focus:true}],labels:[{x:180,y:202,text:'Same durability order.'}]}
];
export const bypass: Panel[] = [
 {title:'Before · discovery stops at the write',description:'A capacity-blocked write at the guest queue head prevents discovery of an independent read behind it.',h:230,
 boxes:[box(20,50,145,'Write','capacity blocked','focus'),box(195,50,145,'Read','not discovered','muted')],edges:[],labels:[{x:180,y:28,text:'guest queue'},{x:180,y:150,text:'The read needs no WAL space.'},{x:180,y:178,text:'It still cannot reach admission.'}]},
 {title:'After · discover, then select',description:'CAS retains a bounded set of request descriptors, then selects independent reads. It preserves overlapping-write dependencies and FLUSH barriers.',h:230,
 boxes:[box(20,50,145,'Write','still pending'),box(195,50,145,'Read','independent','focus'),box(195,152,145,'Read admission',undefined,'focus')],edges:[{d:'M267 102 V152',focus:true}],labels:[{x:180,y:28,text:'retained frontend requests'},{x:91,y:181,text:'payload stays put'}]}
];
export const scheduler: Panel[] = [
 {title:'Before · the refused write stays first',description:'Image A retries a refused write, then image B does the same. On the next visit to either image, the write remains ahead of its eligible read.',h:250,
 boxes:[box(78,35,110,'Write','retry first','focus'),box(220,35,120,'Read','eligible'),box(78,129,110,'Write','retry first','focus'),box(220,129,120,'Read','eligible')],edges:[{d:'M188 61 H220',dashed:true},{d:'M188 155 H220',dashed:true}],labels:[{x:35,y:65,text:'A'},{x:35,y:159,text:'B'},{x:180,y:218,text:'Each visit retries the same head.'}]},
 {title:'After · rotate the refused ticket',description:'A refused, uncommitted scheduler ticket moves behind other tickets in its image. The image selector retains byte-deficit fairness. Mutation publication and FLUSH order do not change.',h:250,
 boxes:[box(78,35,110,'Read','next visit','focus'),box(220,35,120,'Write','retry later'),box(78,129,110,'Read','next visit','focus'),box(220,129,120,'Write','retry later')],edges:[{d:'M188 61 H220'},{d:'M188 155 H220'}],labels:[{x:35,y:65,text:'A'},{x:35,y:159,text:'B'},{x:180,y:218,text:'Scheduler order, not disk order.'}]}
];
export const testbed: Panel[] = [
 {title:'Spark · historical measurements',description:'Workload guests used TCG emulation inside an outer KVM VM. CAS and XFS ran in that VM on a virtual disk backed by a host file. Spark also ran other work.',h:290,
 boxes:[{...box(20,18,320,'',''),h:248,dashed:true},box(60,50,240,'Inner guest','TCG emulation'),box(60,144,240,'CAS + file-backed XFS','inside the outer KVM VM')],edges:[{d:'M180 102 V144'}],labels:[{x:180,y:38,text:'Spark physical host'},{x:180,y:239,text:'Nested development fixture'}]},
 {title:'CloudLab · protocol checks',description:'The guest runs directly under KVM. CAS runs on the physical EPYC host. Completed checks used file-backed XFS on the OS storage path. Dedicated NVMe benchmarks have not run.',h:290,
 boxes:[{...box(20,18,320,'',''),h:248,dashed:true},box(60,50,240,'Guest','direct KVM','focus'),box(60,144,240,'CAS + file-backed XFS','on the physical host')],edges:[{d:'M180 102 V144',focus:true}],labels:[{x:180,y:38,text:'CloudLab EPYC host'},{x:180,y:239,text:'Dedicated NVMe is the next test.'}]}
];
