/// This is a storage abstraction layer over SQLite. Don't use this library
/// directly, it was made specifically for the Turtl app and probably won't ever
/// do the things you want it to.

extern crate jedi;
#[macro_use]
extern crate quick_error;
extern crate rusqlite;
extern crate serde_json;

use ::rusqlite::Connection;
use ::rusqlite::types::Value as SqlValue;
use ::rusqlite::Error as SqlError;
use ::jedi::{Value, JSONError};

pub mod error;

use ::error::{DError, DResult};

/// The Dumpy struct stores our schema and acts as a namespace for our public
/// functions.
pub struct Dumpy {
    schema: Value,
}

impl Dumpy {
    /// Create a new dumpy
    pub fn new(schema: Value) -> Dumpy {
        Dumpy {
            schema: schema,
        }
    }

    /// Init our dumpy store on an existing connection.
    pub fn init(&self, conn: &Connection) -> DResult<()> {
        try!(conn.execute("CREATE TABLE IF NOT EXISTS dumpy_objects (id VARCHAR(64) PRIMARY KEY, table_name VARCHAR(32), data TEXT)", &[]));
        try!(conn.execute("CREATE TABLE IF NOT EXISTS dumpy_index (id ROWID, table_name VARCHAR(32), index_name VARCHAR(32), vals VARCHAR(256), object_id VARCHAR(64))", &[]));
        try!(conn.execute("CREATE TABLE IF NOT EXISTS dumpy_kv (key VARCHAR(32) PRIMARY KEY, value TEXT)", &[]));

        try!(conn.execute("CREATE INDEX IF NOT EXISTS dumpy_idx_index ON dumpy_index (table_name, index_name, vals)", &[]));
        try!(conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS dumpy_idx_kv ON dumpy_kv (key)", &[]));
        Ok(())
    }

    /// Store an object!
    pub fn store(&self, conn: &Connection, table: &String, obj: &Value) -> DResult<()> {
        let id = try!(jedi::get::<String>(&["id"], obj));
        let json = try!(jedi::stringify(obj));
        try!(conn.execute("INSERT INTO dumpy_objects (id, table_name, data) VALUES ($1, $2, $3)", &[&id, table, &json]));

        let indexes = match jedi::get::<Vec<Value>>(&[table, "indexes"], &self.schema) {
            Ok(x) => x,
            Err(e) => match e {
                JSONError::DeadEnd | JSONError::NotFound(..) => {
                    Vec::new()
                },
                _ => return Err(From::from(e)),
            }
        };
        for index in &indexes {
            let idx_name = try!(jedi::get::<String>(&["name"], index));
            let fields = try!(jedi::get::<Vec<String>>(&["fields"], index));
            let mut val_vec: Vec<Vec<String>> = Vec::new();
            let blankval = String::from("");

            // build an array of an array of values (we want all combinations
            // of the various fields)
            for field in &fields {
                let val = jedi::walk(&[&field], &obj);
                let mut subvals: Vec<String> = Vec::new();
                match val {
                    Ok(x) => {
                        match *x {
                            Value::String(ref x) => {
                                subvals.push(x.clone());
                            },
                            Value::I64(ref x) => {
                                subvals.push(format!("{}", x));
                            },
                            Value::U64(ref x) => {
                                subvals.push(format!("{}", x));
                            },
                            Value::F64(ref x) => {
                                subvals.push(format!("{}", x));
                            },
                            Value::Bool(ref x) => {
                                subvals.push(format!("{}", x));
                            },
                            Value::Array(ref x) => {
                                for val in x {
                                    match *val {
                                        Value::String(ref s) => {
                                            subvals.push(s.clone());
                                        }
                                        Value::I64(x) => {
                                            subvals.push(format!("{}", x));
                                        },
                                        Value::U64(x) => {
                                            subvals.push(format!("{}", x));
                                        },
                                        Value::F64(x) => {
                                            subvals.push(format!("{}", x));
                                        },
                                        _ => {
                                            subvals.push(blankval.clone());
                                        },
                                    }
                                }
                            },
                            Value::Null | Value::Object(_) => {
                                subvals.push(blankval.clone());
                            },
                        }
                    },
                    Err(JSONError::NotFound(_)) => {
                        subvals.push(blankval.clone());
                    },
                    Err(e) => return Err(From::from(e)),
                }
                val_vec.push(subvals);
            }

            fn combine(acc: String, next: &Vec<Vec<String>>, final_vals: &mut Vec<String>) {
                if next.len() == 0 {
                    final_vals.push(acc);
                    return;
                }
                let here = &next[0];
                let next = Vec::from(&next[1..]);
                for val in here {
                    let acced;
                    if acc == "" {
                        acced = format!("{}", val);
                    } else {
                        acced = format!("{}|{}", acc, val);
                    }
                    combine(acced, &next, final_vals);
                }

            }
            let mut vals: Vec<String> = Vec::new();
            combine(String::from(""), &val_vec, &mut vals);
            for val in &vals {
                try!(conn.execute("INSERT INTO dumpy_index (table_name, index_name, vals, object_id) VALUES ($1, $2, $3, $4)", &[
                    table,
                    &idx_name,
                    val,
                    &id,
                ]));
            }
        }
        Ok(())
    }

    /// Get an object from dumpy's store
    pub fn get(&self, conn: &Connection, table: &String, id: &String) -> DResult<Value> {
        let query = "SELECT data FROM dumpy_objects WHERE id = $1 AND table_name = $2";
        conn.query_row_and_then(query, &[id, table], |row| -> DResult<Value> {
            let data: SqlValue = try!(row.get_checked("data"));
            match data {
                SqlValue::Text(ref x) => {
                    Ok(try!(jedi::parse(x)))
                },
                _ => Err(DError::Msg(format!("dumpy: {}: {}: `data` field is not a string", table, id))),
            }
        })
    }

    /// Find objects using a given index/values
    pub fn find(&self, conn: &Connection, table: &String, index: &String, vals: &Vec<String>) -> DResult<Vec<Value>> {
        let mut query = try!(conn.prepare("SELECT object_id FROM dumpy_index WHERE table_name = $1 AND index_name = $2 AND vals LIKE $3"));
        let vals_str = vals
            .into_iter()
            .fold(String::new(), |acc, x| {
                if acc == "" {
                    format!("{}", x)
                } else {
                    format!("{}|{}", acc, x)
                }
            });
        let vals_str = format!("{}%", vals_str);
        let rows = try!(query.query_map(&[table, index, &vals_str], |row| {
            row.get("object_id")
        }));
        let mut ids: Vec<String> = Vec::new();
        for oid in rows {
            ids.push(try!(oid));
        }

        let oids = ids.into_iter().fold(String::new(), |acc, x| {
            if acc == "" {
                format!("'{}'", x)
            } else {
                format!("{}, '{}'", acc, x)
            }
        });
        let query = format!("SELECT data FROM dumpy_objects WHERE id IN ({}) ORDER BY id ASC", oids);
        let mut query = try!(conn.prepare(&query[..]));
        let rows = try!(query.query_map(&[], |row| {
            row.get("data")
        }));
        let mut objects: Vec<Value> = Vec::new();
        for data in rows {
            objects.push(try!(jedi::parse(&try!(data))));
        }
        Ok(objects)
    }

    /// Set a value into the key/val store
    pub fn kv_set(&self, conn: &Connection, key: &str, val: &String) -> DResult<()> {
        // TODO finish set kv
        try!(conn.execute("INSERT OR REPLACE INTO dumpy_kv (key, value) VALUES ($1, $2)", &[&key, val]));
        Ok(())
    }

    /// Get a value from the key/val store
    pub fn kv_get(&self, conn: &Connection, key: &str) -> DResult<Option<String>> {
        let query = "SELECT value FROM dumpy_kv WHERE key = $1";
        let res = conn.query_row_and_then(query, &[&key], |row| -> DResult<Option<String>> {
            let data: SqlValue = try!(row.get_checked("value"));
            match data {
                SqlValue::Text(x) => {
                    Ok(Some(x))
                },
                _ => Err(DError::Msg(format!("dumpy: kv: {}: `value` field is not a string", key))),
            }
        });
        match res {
            Ok(x) => Ok(x),
            Err(e) => match e {
                DError::SqlError(e) => match e {
                    SqlError::QueryReturnedNoRows => Ok(None),
                    _ => Err(From::from(e)),
                },
                _ => Err(e),
            },
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use ::jedi;
    use ::rusqlite::Connection;

    fn pre_test() -> (Connection, Dumpy) {
        let conn = Connection::open_in_memory().unwrap();
        let schema = jedi::parse(&String::from(r#"{"boards":null,"notes":{"indexes":[{"name":"boards","fields":["boards"]},{"name":"user_boards","fields":["user_id","boards"]}]}}"#)).unwrap();
        let dumpy = Dumpy::new(schema);
        (conn, dumpy)
    }

    #[test]
    fn inits() {
        let (conn, dumpy) = pre_test();
        dumpy.init(&conn).unwrap();
    }

    #[test]
    fn stores_stuff_gets_stuff() {
        let (conn, dumpy) = pre_test();
        let note = jedi::parse(&String::from(r#"{"id":"abc123","user_id":"andrew123","boards":["1234","5678"],"body":"this is my note lol"}"#)).unwrap();
        dumpy.init(&conn).unwrap();
        dumpy.store(&conn, &String::from("notes"), &note).unwrap();
        let note = dumpy.get(&conn, &String::from("notes"), &String::from("abc123")).unwrap();
        assert_eq!(jedi::get::<String>(&["id"], &note).unwrap(), "abc123");
        assert_eq!(jedi::get::<String>(&["user_id"], &note).unwrap(), "andrew123");
        assert_eq!(jedi::get::<Vec<String>>(&["boards"], &note).unwrap(), vec![String::from("1234"), String::from("5678")]);
        assert_eq!(jedi::get::<String>(&["body"], &note).unwrap(), "this is my note lol");
    }

    #[test]
    fn indexes_and_searches() {
        let (conn, dumpy) = pre_test();
        let note1 = jedi::parse(&String::from(r#"{"id":"n0mnm","user_id":"3443","boards":["1234","5678"],"body":"this is my note lol"}"#)).unwrap();
        let note2 = jedi::parse(&String::from(r#"{"id":"6tuns","user_id":"9823","boards":["1234","2222"],"body":"this is my note lol"}"#)).unwrap();
        let note3 = jedi::parse(&String::from(r#"{"id":"p00pz","user_id":"9823","boards":["5896"],"body":"this is my note lol"}"#)).unwrap();
        let note4 = jedi::parse(&String::from(r#"{"id":"l4cky","user_id":"2938","boards":["3385", "4247"],"body":"this is my note lol"}"#)).unwrap();
        let note5 = jedi::parse(&String::from(r#"{"id":"h4iry","user_id":"4187","boards":["1234"],"body":"this is my note lol"}"#)).unwrap();
        let note6 = jedi::parse(&String::from(r#"{"id":"scl0c","user_id":"4187","body":"this is my note lol"}"#)).unwrap();
        let note7 = jedi::parse(&String::from(r#"{"id":"gr1my","body":"this is my note lol"}"#)).unwrap();
        let board1 = jedi::parse(&String::from(r#"{"id":"s4nd1","title":"get a job"}"#)).unwrap();
        let board2 = jedi::parse(&String::from(r#"{"id":"s4nd2","title":null}"#)).unwrap();
        dumpy.init(&conn).unwrap();
        dumpy.store(&conn, &String::from("notes"), &note1).unwrap();
        dumpy.store(&conn, &String::from("notes"), &note2).unwrap();
        dumpy.store(&conn, &String::from("notes"), &note3).unwrap();
        dumpy.store(&conn, &String::from("notes"), &note4).unwrap();
        dumpy.store(&conn, &String::from("notes"), &note5).unwrap();
        dumpy.store(&conn, &String::from("notes"), &note6).unwrap();
        dumpy.store(&conn, &String::from("notes"), &note7).unwrap();
        dumpy.store(&conn, &String::from("boards"), &board1).unwrap();
        dumpy.store(&conn, &String::from("boards"), &board2).unwrap();

        let notes = dumpy.find(&conn, &String::from("notes"), &String::from("user_boards"), &vec![String::from("9823"), String::from("1234")]).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(jedi::get::<String>(&["id"], &notes[0]).unwrap(), "6tuns");

        let notes = dumpy.find(&conn, &String::from("notes"), &String::from("user_boards"), &vec![String::from("9823")]).unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(jedi::get::<String>(&["id"], &notes[0]).unwrap(), "6tuns");
        assert_eq!(jedi::get::<String>(&["id"], &notes[1]).unwrap(), "p00pz");

        let notes = dumpy.find(&conn, &String::from("notes"), &String::from("boards"), &vec![String::from("1234")]).unwrap();
        assert_eq!(notes.len(), 3);
        assert_eq!(jedi::get::<String>(&["id"], &notes[0]).unwrap(), "6tuns");
        assert_eq!(jedi::get::<String>(&["id"], &notes[1]).unwrap(), "h4iry");
        assert_eq!(jedi::get::<String>(&["id"], &notes[2]).unwrap(), "n0mnm");
    }

    #[test]
    fn kv_set_get() {
        let (conn, dumpy) = pre_test();
        dumpy.init(&conn).unwrap();
        dumpy.kv_set(&conn, "some_setting", &String::from("I AM ABOVE THE LAW")).unwrap();
        let val = dumpy.kv_get(&conn, "some_setting").unwrap();
        assert_eq!(val.unwrap(), "I AM ABOVE THE LAW");

        dumpy.kv_set(&conn, "some_setting", &String::from("i got no feelin'")).unwrap();
        let val = dumpy.kv_get(&conn, "some_setting").unwrap();
        assert_eq!(val.unwrap(), "i got no feelin'");

        let val = dumpy.kv_get(&conn, "doesnt_exist").unwrap();
        assert_eq!(val, None);
    }
}