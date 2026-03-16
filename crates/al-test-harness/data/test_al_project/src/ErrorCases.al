codeunit 50102 "Error Cases"
{
    procedure MissingEnd()
    begin
        Message('hello');

    procedure NoSemicolon()
    begin
        x := 1
    end;

    procedure ExtraBegin()
    begin
        begin
        end;
    end;
}
