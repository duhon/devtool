//! What the payload completion knows of MongoDB: operators, stages and the fields of
//! the commands, each with a short description.

pub type Table = [(&'static str, &'static str)];

/// A name, its description and the text after `"name": `; `$0` is where the cursor goes.
pub type Snippets = [(&'static str, &'static str, &'static str)];

pub const QUERY_OPERATORS: &Snippets = &[
    ("$eq", "Equals", "$0"),
    ("$ne", "Not equal", "$0"),
    ("$gt", "Greater than", "$0"),
    ("$gte", "Greater than or equal", "$0"),
    ("$lt", "Less than", "$0"),
    ("$lte", "Less than or equal", "$0"),
    ("$in", "Matches any value of the array", "[$0]"),
    ("$nin", "Matches none of the values", "[$0]"),
    ("$exists", "The field exists", "$0"),
    ("$type", "The field has the BSON type", "\"$0\""),
    ("$regex", "Matches a regular expression", "\"$0\""),
    ("$options", "Flags of $regex", "\"$0\""),
    ("$not", "Negates an operator expression", "{$0}"),
    (
        "$elemMatch",
        "An array element matches all conditions",
        "{$0}",
    ),
    ("$size", "The array has this length", "$0"),
    ("$all", "The array holds all values", "[$0]"),
    ("$mod", "Remainder of a division", "[$0]"),
    ("$bitsAllSet", "All bit positions are 1", "[$0]"),
    ("$bitsAnySet", "Any bit position is 1", "[$0]"),
    ("$bitsAllClear", "All bit positions are 0", "[$0]"),
    ("$bitsAnyClear", "Any bit position is 0", "[$0]"),
];

pub const LOGICAL: &Snippets = &[
    ("$and", "All conditions hold", "[{$0}]"),
    ("$or", "Any condition holds", "[{$0}]"),
    ("$nor", "No condition holds", "[{$0}]"),
    ("$expr", "Aggregation expression as a condition", "{$0}"),
    ("$text", "Text search", "{\"$search\": \"$0\"}"),
    ("$where", "JavaScript condition", "\"$0\""),
    ("$jsonSchema", "Matches a JSON schema", "{$0}"),
    ("$comment", "Comment of the query", "\"$0\""),
];

pub const UPDATE_OPERATORS: &Table = &[
    ("$set", "Sets fields"),
    ("$unset", "Removes fields"),
    ("$inc", "Adds to numbers"),
    ("$mul", "Multiplies numbers"),
    ("$min", "Lowers to a smaller value"),
    ("$max", "Raises to a larger value"),
    ("$rename", "Renames fields"),
    ("$currentDate", "Sets the current date"),
    ("$setOnInsert", "Sets fields when an upsert inserts"),
    ("$push", "Appends to arrays"),
    ("$pull", "Removes matching array elements"),
    ("$pop", "Removes the first or last array element"),
    ("$addToSet", "Adds to arrays without duplicates"),
    ("$pullAll", "Removes listed array values"),
    ("$bit", "Bitwise update"),
];

/// Aggregation stages: name, description and the body of the stage.
pub const STAGES: &Snippets = &[
    ("$match", "Filters documents", "{$0}"),
    (
        "$group",
        "Groups documents and accumulates",
        "{\"_id\": \"$0\"}",
    ),
    (
        "$project",
        "Includes, excludes or computes fields",
        "{\"$0\": 1}",
    ),
    ("$sort", "Sorts documents", "{\"$0\": 1}"),
    ("$limit", "Keeps the first documents", "$0"),
    ("$skip", "Skips documents", "$0"),
    ("$unwind", "Deconstructs an array field", "\"$$0\""),
    (
        "$lookup",
        "Joins with another collection",
        "{\"from\": \"$0\", \"localField\": \"\", \"foreignField\": \"\", \"as\": \"\"}",
    ),
    ("$addFields", "Adds computed fields", "{\"$0\": \"\"}"),
    ("$set", "Adds computed fields", "{\"$0\": \"\"}"),
    ("$unset", "Removes fields", "\"$0\""),
    ("$count", "Counts documents into a field", "\"$0\""),
    ("$sample", "Picks random documents", "{\"size\": $0}"),
    ("$sortByCount", "Groups by a value and counts", "\"$$0\""),
    (
        "$facet",
        "Several pipelines over the same input",
        "{\"$0\": []}",
    ),
    (
        "$bucket",
        "Groups by boundaries",
        "{\"groupBy\": \"$$0\", \"boundaries\": [], \"default\": \"\", \"output\": {}}",
    ),
    (
        "$bucketAuto",
        "Groups into a number of buckets",
        "{\"groupBy\": \"$$0\", \"buckets\": 5}",
    ),
    (
        "$replaceRoot",
        "Replaces the document",
        "{\"newRoot\": \"$$0\"}",
    ),
    ("$replaceWith", "Replaces the document", "\"$$0\""),
    ("$out", "Writes the result to a collection", "\"$0\""),
    (
        "$merge",
        "Merges the result into a collection",
        "{\"into\": \"$0\"}",
    ),
    (
        "$unionWith",
        "Adds the documents of a collection",
        "{\"coll\": \"$0\", \"pipeline\": []}",
    ),
    (
        "$graphLookup",
        "Recursive search in a collection",
        "{\"from\": \"$0\", \"startWith\": \"$\", \"connectFromField\": \"\", \"connectToField\": \"\", \"as\": \"\"}",
    ),
    ("$geoNear", "Orders by distance", "{$0}"),
    ("$redact", "Restricts content by document fields", "{$0}"),
    ("$setWindowFields", "Window functions", "{$0}"),
    ("$densify", "Fills gaps in a sequence", "{$0}"),
    ("$fill", "Fills missing values", "{$0}"),
    ("$indexStats", "Usage of the indexes", "{}"),
    ("$collStats", "Statistics of the collection", "{$0}"),
    ("$currentOp", "Active operations", "{$0}"),
    ("$listSessions", "Sessions", "{$0}"),
    ("$search", "Atlas Search", "{$0}"),
];

/// Variants of a stage that complete next to its main form: name, label, body.
pub const STAGE_VARIANTS: &Snippets = &[(
    "$lookup",
    "$lookup (pipeline)",
    "{\"from\": \"$0\", \"let\": {}, \"pipeline\": [], \"as\": \"\"}",
)];

pub const ACCUMULATORS: &Table = &[
    ("$sum", "Sum"),
    ("$avg", "Average"),
    ("$min", "Smallest value"),
    ("$max", "Largest value"),
    ("$first", "First value of the group"),
    ("$last", "Last value of the group"),
    ("$push", "Array of the values"),
    ("$addToSet", "Array of the distinct values"),
    ("$count", "Number of documents"),
    ("$stdDevPop", "Population standard deviation"),
    ("$stdDevSamp", "Sample standard deviation"),
    ("$top", "Top element by an order"),
    ("$bottom", "Bottom element by an order"),
    ("$firstN", "First n values"),
    ("$lastN", "Last n values"),
    ("$mergeObjects", "Merges documents"),
];

/// Expression operators: name, description and the body.
pub const EXPRESSIONS: &Snippets = &[
    ("$abs", "Absolute value", "\"$0\""),
    ("$add", "Sum of numbers or dates", "[$0]"),
    ("$ceil", "Rounds up", "\"$0\""),
    ("$divide", "Divides", "[$0]"),
    ("$exp", "e to a power", "\"$0\""),
    ("$floor", "Rounds down", "\"$0\""),
    ("$ln", "Natural logarithm", "\"$0\""),
    ("$log", "Logarithm", "[$0]"),
    ("$log10", "Base 10 logarithm", "\"$0\""),
    ("$mod", "Remainder", "[$0]"),
    ("$multiply", "Product", "[$0]"),
    ("$pow", "Power", "[$0]"),
    ("$round", "Rounds to a number of places", "[\"$0\", 2]"),
    ("$sqrt", "Square root", "\"$0\""),
    ("$subtract", "Difference", "[$0]"),
    ("$trunc", "Truncates", "[\"$0\"]"),
    ("$eq", "Equals", "[$0]"),
    ("$ne", "Not equal", "[$0]"),
    ("$gt", "Greater than", "[$0]"),
    ("$gte", "Greater than or equal", "[$0]"),
    ("$lt", "Less than", "[$0]"),
    ("$lte", "Less than or equal", "[$0]"),
    ("$cmp", "Compares: -1, 0 or 1", "[$0]"),
    ("$and", "All true", "[$0]"),
    ("$or", "Any true", "[$0]"),
    ("$not", "Negates", "[$0]"),
    (
        "$cond",
        "If, then, else",
        "{\"if\": $0, \"then\": \"\", \"else\": \"\"}",
    ),
    (
        "$ifNull",
        "The first value that is not null",
        "[\"$0\", \"\"]",
    ),
    (
        "$switch",
        "Cases with a default",
        "{\"branches\": [{\"case\": $0, \"then\": \"\"}], \"default\": \"\"}",
    ),
    ("$size", "Length of an array", "\"$0\""),
    ("$arrayElemAt", "Element at an index", "[\"$0\", 0]"),
    ("$concatArrays", "Joins arrays", "[$0]"),
    (
        "$filter",
        "Array elements that match",
        "{\"input\": \"$0\", \"as\": \"item\", \"cond\": {}}",
    ),
    (
        "$map",
        "Transforms array elements",
        "{\"input\": \"$0\", \"as\": \"item\", \"in\": \"\"}",
    ),
    (
        "$reduce",
        "Folds an array",
        "{\"input\": \"$0\", \"initialValue\": 0, \"in\": {}}",
    ),
    ("$in", "Value is in an array", "[$0]"),
    ("$first", "First array element", "\"$0\""),
    ("$last", "Last array element", "\"$0\""),
    ("$slice", "Part of an array", "[\"$0\", 1]"),
    ("$reverseArray", "Reverses an array", "\"$0\""),
    ("$isArray", "Is an array", "[$0]"),
    ("$range", "Sequence of numbers", "[$0]"),
    ("$arrayToObject", "Array of pairs to a document", "\"$0\""),
    ("$objectToArray", "Document to an array of pairs", "\"$0\""),
    ("$setUnion", "Union of sets", "[$0]"),
    ("$setIntersection", "Intersection of sets", "[$0]"),
    ("$setDifference", "Difference of sets", "[$0]"),
    ("$concat", "Joins strings", "[$0]"),
    ("$substrCP", "Substring", "[\"$0\", 0, 1]"),
    ("$toLower", "Lower case", "\"$0\""),
    ("$toUpper", "Upper case", "\"$0\""),
    ("$trim", "Trims characters", "{\"input\": \"$0\"}"),
    ("$split", "Splits a string", "[\"$0\", \",\"]"),
    ("$strLenCP", "Length of a string", "\"$0\""),
    (
        "$regexMatch",
        "Matches a regular expression",
        "{\"input\": \"$0\", \"regex\": \"\"}",
    ),
    (
        "$replaceAll",
        "Replaces all matches",
        "{\"input\": \"$0\", \"find\": \"\", \"replacement\": \"\"}",
    ),
    ("$year", "Year of a date", "\"$0\""),
    ("$month", "Month of a date", "\"$0\""),
    ("$dayOfMonth", "Day of the month", "\"$0\""),
    ("$dayOfWeek", "Day of the week", "\"$0\""),
    ("$hour", "Hour of a date", "\"$0\""),
    ("$minute", "Minute of a date", "\"$0\""),
    (
        "$dateToString",
        "Formats a date",
        "{\"format\": \"%Y-%m-%d\", \"date\": \"$0\"}",
    ),
    (
        "$dateFromString",
        "Parses a date",
        "{\"dateString\": \"$0\"}",
    ),
    (
        "$dateDiff",
        "Difference of two dates",
        "{\"startDate\": \"$0\", \"endDate\": \"\", \"unit\": \"day\"}",
    ),
    ("$toDate", "Converts to a date", "\"$0\""),
    ("$toString", "Converts to a string", "\"$0\""),
    ("$toInt", "Converts to an int", "\"$0\""),
    ("$toLong", "Converts to a long", "\"$0\""),
    ("$toDouble", "Converts to a double", "\"$0\""),
    ("$toDecimal", "Converts to a decimal", "\"$0\""),
    ("$toBool", "Converts to a boolean", "\"$0\""),
    ("$toObjectId", "Converts to an ObjectId", "\"$0\""),
    ("$type", "BSON type of a value", "\"$0\""),
    (
        "$convert",
        "Converts a value",
        "{\"input\": \"$0\", \"to\": \"\"}",
    ),
    ("$isNumber", "Is a number", "\"$0\""),
    ("$sum", "Sum", "\"$0\""),
    ("$avg", "Average", "\"$0\""),
    ("$min", "Smallest value", "\"$0\""),
    ("$max", "Largest value", "\"$0\""),
    ("$mergeObjects", "Merges documents", "[$0]"),
    ("$getField", "Value of a field", "\"$0\""),
    (
        "$setField",
        "Sets a field",
        "{\"field\": \"$0\", \"input\": \"$$ROOT\", \"value\": \"\"}",
    ),
    ("$literal", "A value without evaluation", "$0"),
    (
        "$let",
        "Variables for an expression",
        "{\"vars\": {}, \"in\": $0}",
    ),
    ("$rand", "Random number", "{}"),
];

pub const SYSTEM_VARIABLES: &Table = &[
    ("$$ROOT", "The top-level document"),
    ("$$CURRENT", "The current document"),
    ("$$NOW", "The current date"),
    ("$$CLUSTER_TIME", "The cluster time"),
    ("$$REMOVE", "Removes the field"),
    ("$$DESCEND", "Keeps the level and descends"),
    ("$$PRUNE", "Drops the level"),
    ("$$KEEP", "Keeps the level"),
];

pub const COMMANDS: &Table = &[
    ("ping", "Checks the connection"),
    ("find", "Queries documents"),
    ("aggregate", "Runs a pipeline"),
    ("count", "Counts documents"),
    ("distinct", "Distinct values"),
    ("insert", "Inserts documents"),
    ("update", "Updates documents"),
    ("delete", "Deletes documents"),
    ("findAndModify", "Updates or removes one document"),
    ("createIndexes", "Creates indexes"),
    ("dropIndexes", "Drops indexes"),
    ("listIndexes", "Lists indexes"),
    ("listCollections", "Lists collections"),
    ("listDatabases", "Lists databases"),
    ("create", "Creates a collection"),
    ("drop", "Drops a collection"),
    ("dropDatabase", "Drops the database"),
    ("collStats", "Collection statistics"),
    ("dbStats", "Database statistics"),
    ("serverStatus", "Server status"),
    ("hello", "Server role and topology"),
    ("buildInfo", "Server version"),
    ("explain", "Query plan"),
    ("connectionStatus", "Current user and roles"),
];

/// Commands whose value is a collection name.
pub const COLLECTION_COMMANDS: &[&str] = &[
    "find",
    "aggregate",
    "count",
    "distinct",
    "insert",
    "update",
    "delete",
    "findAndModify",
    "createIndexes",
    "dropIndexes",
    "listIndexes",
    "create",
    "drop",
    "collStats",
];

pub const INDEX_OPTIONS: &Snippets = &[
    ("key", "Fields and directions", "{$0}"),
    ("name", "Name of the index", "\"$0\""),
    ("unique", "No duplicates", "$0"),
    ("sparse", "Only documents with the field", "$0"),
    ("expireAfterSeconds", "Time to live", "$0"),
    ("partialFilterExpression", "Only matching documents", "{$0}"),
    ("collation", "Collation", "{$0}"),
    ("hidden", "Hidden from the planner", "$0"),
    ("background", "Build in the background", "$0"),
];

pub const TYPES: &[&str] = &[
    "double",
    "string",
    "object",
    "array",
    "binData",
    "objectId",
    "bool",
    "date",
    "null",
    "regex",
    "int",
    "timestamp",
    "long",
    "decimal",
    "number",
    "minKey",
    "maxKey",
];

pub const INDEX_DIRECTIONS: &[&str] = &["text", "hashed", "2dsphere", "2d"];
pub const BOOLEANS: &[&str] = &["true", "false"];
pub const DIRECTIONS: &[&str] = &["1", "-1"];
pub const PROJECTIONS: &[&str] = &["1", "0", "true", "false"];
pub const REGEX_OPTIONS: &[&str] = &["i", "m", "x", "s"];
pub const MERGE_MATCHED: &[&str] = &["replace", "keepExisting", "merge", "fail", "pipeline"];
pub const MERGE_NOT_MATCHED: &[&str] = &["insert", "discard", "fail"];
pub const VERBOSITY: &[&str] = &["queryPlanner", "executionStats", "allPlansExecution"];
pub const GRANULARITY: &[&str] = &[
    "R5", "R10", "R20", "R40", "R80", "1-2-5", "E6", "E12", "E24",
];
