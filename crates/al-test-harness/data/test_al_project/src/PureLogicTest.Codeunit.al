codeunit 50110 "Pure Logic Test"
{
    Subtype = Test;

    [Test]
    procedure TestAddition()
    var
        Result: Integer;
    begin
        // Verify simple arithmetic — no UI, no Library Assert dependency
        Result := 2 + 2;
        if Result <> 4 then
            Error('Expected 4, got %1', Result);
    end;

    [Test]
    procedure TestStringConcat()
    var
        Combined: Text;
    begin
        Combined := 'Hello' + ' ' + 'World';
        if Combined <> 'Hello World' then
            Error('Unexpected concat result: %1', Combined);
    end;

    procedure HelperNotATest()
    begin
        // This procedure has no [Test] attribute — must NOT appear in discovery results
    end;
}
