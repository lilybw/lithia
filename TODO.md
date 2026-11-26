Remaining before lithia v1.0 (loosily in order)

* Modernization pass of used methods, algorithms, and models (see if newer / better)
    * For instance Tokio might not be the best async runtime. Reevaluate
* Codebase simplification and restructuring pass
* Dependency version check pass (see if newer / better)
* Sync pass between spec and implementation
* Testing and validation pass 
* Release configuration and build of release binaries. Including making aforementioned artifacts publically available. 
* Make repo public


Inbound functionality:
* onCleanup-like functionality to queue any function body to run upon a nodes ghosting/disposal. Naught but a node.upon(teardown, |node| {...}).. although thatd make like per-node events and stuff which gets messy. If node! is general purpose and fulfills the role of "uponParentExec/any", then it might warrant a seperate macro like cleanup!(|...nodes| {...}) to keep node! from not getting anymore complicated, and to allow for the enforcement of various constraints now that user intent can be assumed and thus the API can be restricted. Preferably, implementation-wise, its but a new edge in the graph using exactly the same behaviour as everything else and thus internally consistent. But given that it might be but a different type of edge, maybe edge type is concise enough to warrent place within node! itself like: node!(|...nodes| {...}, when = TEARDOWN), where the deafult is ANY, and with "equality = |old: &K, new: &K| bool {...}" provided "when" becomes ON_CHANGE... Making when inferred in most cases. Decisions decisions... gotta see what pros and cons it might have. Supporting multiple edge types would also make UPON_FAILURE-like edges available which could be very useful. And given that yield! is introduced to let any node emit any amount of values during the execution of its body, some very nifty architecture could become possible by having a node activate different edge types intentionally...   
* Research Scaffolding for feature allowing for persistant, reused, nodes created in the bodies of other nodes, skipping de-allocation and re-creation on node! site execution and parent scope exit respectively. Such feature needs a way for the user to define node identity as a function of use-case rather than engine-side node identity. 
React does this by requiring explicit keying in loops, and this use-case-identity is likewise required in any iteration to allow a node created in response to some item in some collection to be re-identified and updated in place rather than reconstructing everything. 
* Scaffolding for store-like functionality. Derive macro perchance?

v1.1
* Maintenance burden reduction by attempted allocation of functional reponsibilities onto other crates
