codeunit 50100 "Hello World"
{
    // TODO: implement properly
    procedure DoSomething(a: Integer; b: Integer; c: Integer; d: Integer; e: Integer; f: Integer; g: Integer; h: Integer)
    var
        x: Integer;
    begin
    end;

    procedure badName()
    begin
        Message('Hello World');
    end;

    trigger OnRun()
    begin
    end;
}
