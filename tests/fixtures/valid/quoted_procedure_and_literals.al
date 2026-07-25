codeunit 50100 "Fixture Literals"
{
    procedure "Quoted Proc"(SomeDate: Date; SomeTime: Time; SomeDateTime: DateTime)
    var
        LocalDate: Date;
        LocalTime: Time;
        LocalDateTime: DateTime;
    begin
        LocalDate := 20201231D;
        LocalTime := 010100T;
        LocalDateTime := 0DT;
        SomeDateTime := CreateDateTime(20200101D, 010203T);
    end;
}
